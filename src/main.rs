use std::path::PathBuf;

use clap::{Parser, Subcommand};
use concordance::{
    api::CreateProposalRequest,
    auth::{inspect_jwt, require_valid_jwt},
    client::EkklesiaClient,
    proposal,
    proposal::ProposalDocument,
    render::{render_proposal_md, title_to_slug},
    store::{InstanceConfig, Store},
};

#[derive(Parser)]
#[command(
    name = "concordance",
    about = "LLM-mediated client for the Ekklesia governance API",
    version,
    before_help = concordance::BANNER,
    arg_required_else_help = true,
)]
struct Cli {
    #[arg(
        long,
        short,
        global = true,
        help = "Instance name (defaults to the configured default)"
    )]
    instance: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage ekklesia instances
    #[command(subcommand)]
    Instances(InstancesCmd),

    /// Manage JWT authentication tokens
    #[command(subcommand)]
    Auth(AuthCmd),

    /// List voting events
    Votes {
        #[arg(long, default_value = "20")]
        limit: u32,
    },

    /// Manage proposals
    #[command(subcommand)]
    Proposals(ProposalsCmd),

    /// Manage comments
    #[command(subcommand)]
    Comments(CommentsCmd),

    /// Run the MCP server over stdio (for use by LLM agents)
    Mcp,
}

// ── Instances ─────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum InstancesCmd {
    /// Add an ekklesia instance
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// List configured instances
    List,
    /// Remove an instance
    Remove { name: String },
    /// Set the default instance
    Default { name: String },
}

// ── Auth ──────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum AuthCmd {
    /// Store a JWT token for an instance (get it from the browser cookie named 'token')
    Set {
        #[arg(long)]
        jwt: String,
    },
    /// Show token status for an instance
    Status,
    /// Remove stored token
    Clear,
}

// ── Proposals ─────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ProposalsCmd {
    /// List proposals for a vote
    List {
        #[arg(long)]
        vote: String,
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Show a single proposal as markdown
    Get { id: String },
    /// Fetch all proposals for a vote and save as markdown files
    Save {
        #[arg(long)]
        vote: String,
        #[arg(long, default_value = "analysis/proposals")]
        output: PathBuf,
    },
    /// Submit or update a proposal from a markdown file
    Submit {
        file: PathBuf,
        #[arg(long, help = "Build payload but do not send")]
        dry_run: bool,
    },
}

// ── Comments ──────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum CommentsCmd {
    /// List comments on a proposal
    List {
        #[arg(long)]
        proposal: String,
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Post a comment on a proposal
    Add {
        #[arg(long)]
        proposal: String,
        message: String,
        #[arg(long, help = "Reply to this comment ID")]
        parent: Option<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let store = Store::open()?;

    // dry-run submit needs no client or instance config
    if let Commands::Proposals(ProposalsCmd::Submit {
        ref file,
        dry_run: true,
    }) = cli.command
    {
        let content = std::fs::read_to_string(file)?;
        let doc = proposal::parse_document(&content)?;
        let req = proposal::build_request(&doc);
        print_proposal_preview(&doc, &req)?;
        println!("Dry run — not submitting.");
        return Ok(());
    }

    match cli.command {
        Commands::Instances(cmd) => handle_instances(&store, cmd)?,
        Commands::Auth(cmd) => handle_auth(&store, cli.instance, cmd)?,
        Commands::Votes { limit } => {
            let (name, client) = make_client(&store, cli.instance.as_deref())?;
            handle_votes(&client, &name, limit).await?;
        }
        Commands::Proposals(cmd) => {
            let (name, client) = make_client(&store, cli.instance.as_deref())?;
            handle_proposals(&store, &client, &name, cmd).await?;
        }
        Commands::Comments(cmd) => {
            let (name, client) = make_client(&store, cli.instance.as_deref())?;
            handle_comments(&client, &name, cmd).await?;
        }
        Commands::Mcp => {
            concordance::mcp::run_stdio(store).await?;
        }
    }
    Ok(())
}

// ── Command handlers ──────────────────────────────────────────────────────────

fn handle_instances(store: &Store, cmd: InstancesCmd) -> anyhow::Result<()> {
    match cmd {
        InstancesCmd::Add { url, name } => {
            let name = name.unwrap_or_else(|| instance_name_from_url(&url));
            store.add_instance(&InstanceConfig {
                name: name.clone(),
                url: url.clone(),
            })?;
            println!("Added instance '{name}' → {url}");
        }
        InstancesCmd::List => {
            let instances = store.list_instances()?;
            let default = store.default_instance().ok();
            if instances.is_empty() {
                println!("No instances configured.");
            }
            for inst in &instances {
                let marker = if default.as_deref() == Some(&inst.name) {
                    " (default)"
                } else {
                    ""
                };
                println!("  {}  {}{}", inst.name, inst.url, marker);
            }
        }
        InstancesCmd::Remove { name } => {
            store.remove_instance(&name)?;
            println!("Removed instance '{name}'");
        }
        InstancesCmd::Default { name } => {
            store.set_default_instance(&name)?;
            println!("Default instance set to '{name}'");
        }
    }
    Ok(())
}

fn handle_auth(store: &Store, instance: Option<String>, cmd: AuthCmd) -> anyhow::Result<()> {
    let name = resolve_instance(store, instance.as_deref())?;
    match cmd {
        AuthCmd::Set { jwt } => {
            inspect_jwt(&jwt)?; // validate it's a real JWT before storing
            store.set_token(&name, &jwt)?;
            println!("Token stored for '{name}'");
        }
        AuthCmd::Status => {
            let jwt = store.get_token(&name)?;
            let info = inspect_jwt(&jwt)?;
            println!("{name}: {}", info.status_line());
        }
        AuthCmd::Clear => {
            store.remove_token(&name)?;
            println!("Token cleared for '{name}'");
        }
    }
    Ok(())
}

async fn handle_votes(client: &EkklesiaClient, instance: &str, limit: u32) -> anyhow::Result<()> {
    let page = client.list_votes(1, limit).await?;
    println!("Votes on '{instance}' ({} total):", page.meta.total);
    for vote in &page.data {
        println!(
            "  {}  {}  [{}]",
            vote.id,
            vote.title,
            vote.form.as_deref().unwrap_or("?")
        );
    }
    Ok(())
}

async fn handle_proposals(
    _store: &Store,
    client: &EkklesiaClient,
    instance: &str,
    cmd: ProposalsCmd,
) -> anyhow::Result<()> {
    match cmd {
        ProposalsCmd::List { vote, limit } => {
            let page = client.list_proposals(&vote, None, 1, limit).await?;
            println!(
                "Proposals on '{instance}' for vote {vote} ({} total):",
                page.meta.total
            );
            for p in &page.data {
                println!("  {}  [{}]  {}", p.id, p.status, p.title);
            }
        }
        ProposalsCmd::Get { id } => {
            let p = client.get_proposal(&id).await?;
            print!("{}", render_proposal_md(&p));
        }
        ProposalsCmd::Save { vote, output } => {
            std::fs::create_dir_all(&output)?;
            let mut page = 1u32;
            let mut saved = 0u32;
            loop {
                let result = client.list_proposals(&vote, None, page, 50).await?;
                let total_pages = result.meta.total_pages;
                for stub in &result.data {
                    let p = client.get_proposal(&stub.id).await?;
                    let slug = title_to_slug(&p.title);
                    let path = output.join(format!("{slug}.md"));
                    std::fs::write(&path, render_proposal_md(&p))?;
                    saved += 1;
                    println!("  [{saved}] {slug}.md");
                }
                if page >= total_pages {
                    break;
                }
                page += 1;
            }
            println!("Saved {saved} proposals to {}", output.display());
        }
        ProposalsCmd::Submit { file, dry_run: _ } => {
            let content = std::fs::read_to_string(&file)?;
            let doc = proposal::parse_document(&content)?;
            let req = proposal::build_request(&doc);
            print_proposal_preview(&doc, &req)?;

            let proposal_id = doc.frontmatter.api.proposal_id.as_deref();
            let result = if let Some(id) = proposal_id {
                client.update_proposal(id, &req).await?
            } else {
                client.create_proposal(&req).await?
            };

            let id = result["id"].as_str().or(proposal_id).unwrap_or("unknown");
            let version = result["version"].as_u64().unwrap_or(0);
            let updated_at = result["updatedAt"].as_str().unwrap_or("unknown");
            let vote_slug = client
                .get_vote(&req.vote_id)
                .await
                .map(|v| v.slug)
                .unwrap_or_else(|_| req.vote_id.clone());
            let link = format!("{}/votes/{vote_slug}/{id}", client.base_url());
            println!("Submitted successfully.");
            println!("  id:         {id}");
            println!("  version:    {version}");
            println!("  updated at: {updated_at}");
            println!("  link:       {link}");
        }
    }
    Ok(())
}

async fn handle_comments(
    client: &EkklesiaClient,
    _instance: &str,
    cmd: CommentsCmd,
) -> anyhow::Result<()> {
    match cmd {
        CommentsCmd::List { proposal, limit } => {
            let page = client.list_comments(&proposal, 1, limit).await?;
            println!("Comments ({} total):", page.meta.total);
            for c in &page.data {
                let author = c
                    .author
                    .as_ref()
                    .and_then(|a| a.name.as_deref())
                    .unwrap_or("?");
                println!("  {} [{}]: {}", c.id, author, c.content);
                if c.reply_count.unwrap_or(0) > 0 {
                    println!("    ({} replies)", c.reply_count.unwrap());
                }
            }
        }
        CommentsCmd::Add {
            proposal,
            message,
            parent,
        } => {
            use concordance::api::CreateCommentRequest;
            let req = CreateCommentRequest {
                proposal_id: proposal,
                content: message,
                parent_id: parent,
            };
            let result = client.create_comment(&req).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }
    Ok(())
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn resolve_instance(store: &Store, override_name: Option<&str>) -> anyhow::Result<String> {
    Ok(match override_name {
        Some(n) => n.to_string(),
        None => store.default_instance()?,
    })
}

fn make_client(
    store: &Store,
    override_name: Option<&str>,
) -> anyhow::Result<(String, EkklesiaClient)> {
    let name = resolve_instance(store, override_name)?;
    let config = store.get_instance(&name)?;
    let jwt = store.get_token(&name)?;
    require_valid_jwt(&jwt, &name)?;
    let client = EkklesiaClient::new(&config.url, &jwt)?;
    Ok((name, client))
}

fn print_proposal_preview(
    doc: &ProposalDocument,
    req: &CreateProposalRequest,
) -> anyhow::Result<()> {
    println!("Summary:      {} chars (limit 2000)", doc.summary.len());
    println!(
        "Track record: {} chars (limit 5000)",
        doc.track_record.len()
    );
    println!("Work packages: {}", doc.work_packages.len());
    for wp in &doc.work_packages {
        println!(
            "  [{}]  milestones={}  budget_items={}",
            wp.name,
            wp.milestones.len(),
            wp.budget_breakdown.len()
        );
    }
    println!("Total budget: {:>12} ADA", format_ada(doc.total_budget));
    let payload_json = serde_json::to_string_pretty(req)?;
    std::fs::write("/tmp/proposal_payload.json", &payload_json)?;
    println!("\nPayload written to /tmp/proposal_payload.json");
    Ok(())
}

fn instance_name_from_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

fn format_ada(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}
