use serde::{
    de::{self, DeserializeSeed},
    Deserializer,
};

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
