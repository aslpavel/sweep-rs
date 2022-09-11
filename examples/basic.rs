use anyhow::Error;
use sweep::sweep;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let items: Vec<_> = ["One", "Two", "Three", "Four", "Five"]
        .into_iter()
        .map(|e| e.to_owned())
        .collect();
    let entry: Option<String> = sweep(items, None).await?;
    println!("{:?}", entry);
    Ok(())
}
