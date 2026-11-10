//! Lists open pull requests using a token supplied by the calling application.

use std::io::{self, Write};

use bb_api::{Client, RepoRef};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::args().nth(1).ok_or("pass workspace/repository")?;
    let repo: RepoRef = repository.parse()?;
    let client = Client::builder()
        .token(std::env::var("BB_TOKEN")?)
        .build()?;
    let pull_requests = client.pull_requests(&repo, r#"state = "OPEN""#, 30).await?;

    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &pull_requests)?;
    writeln!(stdout)?;
    Ok(())
}
