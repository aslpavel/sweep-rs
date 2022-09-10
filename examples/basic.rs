use anyhow::Error;
use sweep::sweep;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let entry: Option<String> = sweep(["one", "two", "three", "four", "five"], None).await?;
    println!("{:?}", entry);
    Ok(())
}
