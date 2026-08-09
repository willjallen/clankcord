fn main() {
    std::process::exit(clankcord::app::cli::main(
        std::env::args().skip(1).collect(),
    ));
}
