use std::convert::Infallible;

use anyhow::Error;
use futures::stream;
use sweep::sweep;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let items = ["One", "Two", "Three", "Four", "Five"]
        .into_iter()
        .map(|e| Ok::<_, Infallible>(e.to_owned()));
    let result = sweep(Default::default(), (), stream::iter(items)).await?;
    println!("{:?}", result);
    Ok(())
}
