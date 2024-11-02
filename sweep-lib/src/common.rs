use std::{
    future::Future,
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};

use arrow_array::{
    builder::{GenericByteViewBuilder, PrimitiveBuilder},
    types::ByteViewType,
    Array, ArrowPrimitiveType, GenericByteViewArray, PrimitiveArray,
};
use arrow_data::ByteView;
use serde::{
    de::{self, DeserializeSeed},
    Deserializer,
};
use tokio::task::JoinHandle;

pub trait LockExt {
    type Value;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out;

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out;
}

impl<V> LockExt for std::sync::Mutex<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out,
    {
        let value = self.lock().expect("lock poisoned");
        scope(&*value)
    }

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.lock().expect("lock poisoned");
        scope(&mut *value)
    }
}

impl<V> LockExt for std::sync::RwLock<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out,
    {
        let value = self.read().expect("lock poisoned");
        scope(&*value)
    }

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.write().expect("lock poisoned");
        scope(&mut *value)
    }
}

/// Aborts task associated with [JoinHandle] on drop
#[derive(Debug)]
pub struct AbortJoinHandle<T> {
    handle: JoinHandle<T>,
}

impl<T> Drop for AbortJoinHandle<T> {
    fn drop(&mut self) {
        self.handle.abort()
    }
}

impl<T> Future for AbortJoinHandle<T> {
    type Output = <JoinHandle<T> as Future>::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle).poll(cx)
    }
}

impl<T> From<JoinHandle<T>> for AbortJoinHandle<T> {
    fn from(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }
}

impl<T> Deref for AbortJoinHandle<T> {
    type Target = JoinHandle<T>;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

#[derive(Clone)]
pub struct VecDeserializeSeed<S>(pub S);

impl<'de, S> DeserializeSeed<'de> for VecDeserializeSeed<S>
where
    S: DeserializeSeed<'de> + Clone,
{
    type Value = Vec<S::Value>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VecVisitor<S> {
            seed: S,
        }

        impl<'de, S> de::Visitor<'de> for VecVisitor<S>
        where
            S: DeserializeSeed<'de> + Clone,
        {
            type Value = Vec<S::Value>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("sequence or null")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element_seed(self.seed.clone())? {
                    items.push(item);
                }
                Ok(items)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Vec::new())
            }
        }

        deserializer.deserialize_any(VecVisitor { seed: self.0 })
    }
}

pub fn json_from_slice_seed<'de, 'a: 'de, S: DeserializeSeed<'de>>(
    seed: S,
    slice: &'a [u8],
) -> serde_json::Result<S::Value> {
    use serde_json::{de::SliceRead, Deserializer};

    let mut deserializer = Deserializer::new(SliceRead::new(slice));
    seed.deserialize(&mut deserializer)
}

/// Efficient [GenericByteViewArray] filter that reuses buffers from input array
pub(crate) fn byte_view_filter<T, P>(
    array: &GenericByteViewArray<T>,
    builder: &mut GenericByteViewBuilder<T>,
    mut predicate: P,
) where
    T: ByteViewType,
    P: FnMut(usize, &T::Native) -> bool,
{
    let buffers = array.data_buffers();
    let buffer_offset = if buffers.is_empty() {
        0
    } else {
        let buffer_offset = builder.append_block(buffers[0].clone());
        for buffer in &buffers[1..] {
            builder.append_block(buffer.clone());
        }
        buffer_offset
    };

    let nulls = array.nulls();
    array.views().iter().enumerate().for_each(|(index, view)| {
        let item = unsafe {
            // Safety: index comes from iterating views
            array.value_unchecked(index)
        };
        if !predicate(index, item) {
            return;
        }
        if nulls.map(|nulls| nulls.is_null(index)).unwrap_or(false) {
            return;
        }
        let view = ByteView::from(*view);
        if view.length <= 12 {
            builder.append_value(item);
        } else {
            unsafe {
                // Safety: view/blocks are taken for source string view array
                builder.append_view_unchecked(
                    buffer_offset + view.buffer_index,
                    view.offset,
                    view.length,
                );
            }
        }
    });
}

pub(crate) fn byte_view_concat<'a, T>(
    arrays: impl IntoIterator<Item = &'a GenericByteViewArray<T>>,
) -> GenericByteViewArray<T>
where
    T: ByteViewType,
{
    let mut builder = GenericByteViewBuilder::new();
    arrays
        .into_iter()
        .for_each(|array| byte_view_filter(array, &mut builder, |_, _| true));
    builder.finish()
}

pub(crate) fn primitive_concat<'a, T>(
    arrays: impl IntoIterator<Item = &'a PrimitiveArray<T>>,
) -> PrimitiveArray<T>
where
    T: ArrowPrimitiveType,
{
    let mut builder = PrimitiveBuilder::new();
    arrays.into_iter().for_each(|array| {
        builder.extend(array.iter());
    });
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::de::StrRead;
    use std::marker::PhantomData;

    #[test]
    fn test_vec_deseed() -> Result<(), anyhow::Error> {
        let mut deserializer = serde_json::Deserializer::new(StrRead::new("[1, 2, 3]"));
        let result = VecDeserializeSeed(PhantomData::<i32>).deserialize(&mut deserializer)?;
        assert_eq!(result, vec![1, 2, 3]);

        let mut deserializer = serde_json::Deserializer::new(StrRead::new("null"));
        let result = VecDeserializeSeed(PhantomData::<i32>).deserialize(&mut deserializer)?;
        assert_eq!(result, Vec::<i32>::new());

        Ok(())
    }
}
