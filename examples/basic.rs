use anyhow::Error;
use sweep::{sweep, StringHaystack};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let entry: Option<StringHaystack> =
        sweep(["one", "two", "three", "four", "five"], None).await?;
    println!("{:?}", entry);
    Ok(())
}
