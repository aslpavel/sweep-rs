use anyhow::Error;
use sweep::{sweep, StringHaystack};

fn main() -> Result<(), Error> {
    let entry: Option<StringHaystack> = sweep(vec!["one", "two", "three", "four", "five"], None)?;
    println!("{:?}", entry);
    Ok(())
}
