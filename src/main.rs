mod app {
    include!("app.rs");
    mod namespace;

    pub(crate) fn entry() -> anyhow::Result<()> {
        if namespace::run_if_requested()? {
            Ok(())
        } else {
            run_main()
        }
    }
}

fn main() -> anyhow::Result<()> {
    app::entry()
}
