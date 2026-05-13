fn main() {
    std::process::exit(clankcord::cli::main(std::env::args().skip(1).collect()));
}
