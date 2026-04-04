use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let source = std::env::args().nth(1).map(PathBuf::from);
    pdf_presenter::app::run(source)
}
