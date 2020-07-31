### Sweep
Sweep is a tool for interactive search through a list of entries. It is inspired by [fzf](https://github.com/junegunn/fzf).

### Feautres
  - Fast
  - Customizable
  - Beautiful
  
### Demo time!
![demo](/resources/demo.gif "demo")

### Installation
  - Clone
  - Install rust toolchain either with the package manager of your choice or with [rustup](https://rustup.rs/)
  - Build and install it with cargo (default installation path is $HOME/.cargo/bin/sweep make sure it is in your $PATH)
  ```
  $ cargo install --path .
  ```
  - Or build it and copy the binary
  ```
  $ cargo build --release
  $ cp target/release/sweep ~/.bin
  ```
  - Test it
  ```
  $ printf "one\ntwo\nthree" | sweep
  ```
  - Enjoy

