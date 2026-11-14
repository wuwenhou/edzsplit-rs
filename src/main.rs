mod archive;
mod cli;
mod commands;
mod error;
mod event;
mod graph;
mod model;
mod payload;
mod selftest;
mod split;
mod util;
mod verify;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cli::run(args));
}
