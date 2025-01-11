use docs_rs::context::BinContext;
use docs_rs_consistency_check as consistency;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ctx = BinContext::new();
    // FIXME: add command line argument parsing
    consistency::run_check(&ctx, false).await?;
    Ok(())
}
