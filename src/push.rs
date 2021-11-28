use anyhow::{anyhow, bail};
use chrono::DateTime;
use colored::Colorize;
use frontmatter::Yaml;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fs, path::PathBuf};
use structopt::StructOpt;
use walkdir::WalkDir;

enum UploadStatus<'a> {
    Uploaded,
    Syncing(&'a Article),
    Posting,
}

impl fmt::Display for UploadStatus<'_> {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let s = format!(
            "{}",
            match self {
                UploadStatus::Uploaded => "UPLOADED".green(),
                UploadStatus::Posting => "POSTING".yellow(),
                UploadStatus::Syncing(ref _remote) => "SYNCING".yellow(),
            }
        );
        f.write_str(&s)
    }
}

#[derive(PartialEq, Debug)]
enum PublishStatus {
    Published,
    Draft,
}

impl fmt::Display for PublishStatus {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let s = format!(
            "{}",
            match self {
                PublishStatus::Published => "published".dimmed(),
                PublishStatus::Draft => "draft".dimmed(),
            }
        );
        f.write_str(&s)
    }
}

#[derive(Debug, Deserialize)]
struct DevtoError {
    error: String,
}

#[derive(Debug, Serialize)]
struct CreateArticleInput {
    body_markdown: String,
}

#[derive(Debug, Deserialize)]
struct Article {
    id: u32,
    title: String,
    description: String,
    cover_image: Option<String>,
    published: bool,
    published_at: Option<String>,
    tag_list: Vec<String>,
    slug: String,
    path: String,
    url: String,
    canonical_url: String,
    published_timestamp: String,
    body_markdown: String,
}

/// A dev.to tool for the road 👩🏽‍💻🎒
///
/// Uploads local markdown files with dev.to
#[derive(StructOpt, Debug)]
pub struct Push {
    /// Directory to source markdown files from. Defaults to current working directory
    #[structopt(short, long)]
    source: Option<PathBuf>,
    /// Run without actually updating account
    #[structopt(short, long)]
    dryrun: bool,
}

fn extract(
    name: &str,
    content: &str,
) -> anyhow::Result<(Frontmatter, String)> {
    let (front, back) = match frontmatter::parse_and_find_content(&content) {
        Ok((front, back)) => (front, back),
        Err(err) => {
            eprintln!("Error extracting front matter from {}", name);
            Err(err)?
        }
    };
    let metadata = front.ok_or_else(
        || {
            anyhow!(
                "file {} is missing required markdown frontmatter.\n  ▶ Please see https://dev.to/p/editor_guide more information on what frontmatter is expected", name
            )
        }
    )?;

    Ok((Frontmatter::from_file(&name, metadata)?, back.into()))
}

/// Markdown frontmatter dev.to api documents as acceptable input
#[derive(Debug, PartialEq, Default)]
struct Frontmatter {
    title: String,
    published: Option<bool>,
    tags: Option<String>,
    date: Option<String>,
    series: Option<String>,
    canonical_url: Option<String>,
    cover_image: Option<String>,
}

impl Frontmatter {
    fn publish_status(&self) -> PublishStatus {
        if self.published.unwrap_or_default() {
            PublishStatus::Published
        } else {
            PublishStatus::Draft
        }
    }
    /// extract and validate raw yaml frontmatter
    fn from_file(
        name: &str,
        metadata: Yaml,
    ) -> anyhow::Result<Frontmatter> {
        let hash = metadata
            .into_hash()
            .ok_or_else(|| anyhow!("file {} contains frontmatter that not well formatted", name))?;
        let string = |name: &str| -> Option<String> {
            hash.get(&Yaml::String(name.into()))
                .and_then(|v| v.as_str().map(|s| s.into()))
        };
        let boolean = |name: &str| -> Option<bool> {
            hash.get(&Yaml::String(name.into()))
                .and_then(|v| v.as_bool())
        };
        let title = string("title")
            .ok_or_else(|| anyhow!("file {} contains frontmatter missing a string title", name))?;
        let published = boolean("published");
        let tags = string("tags");
        let date = string("date");
        if let Some(value) = &date {
            if DateTime::parse_from_rfc3339(&value).is_err() {
                bail!(
                    "file {} contains frontmatter with and invalid date: {}",
                    name,
                    value
                );
            }
        }
        let series = string("series");
        let canonical_url = string("canonical_url");
        let cover_image = string("cover_image");

        Ok(Frontmatter {
            title,
            published,
            tags,
            date,
            series,
            canonical_url,
            cover_image,
        })
    }
}

async fn post(
    client: Client,
    api_key: String,
    content: String,
) -> anyhow::Result<()> {
    again::retry(move || {
        let client = client.clone();
        let api_key = api_key.clone();
        let content = content.clone();
        async move {
            let resp = client
                .post("https://dev.to/api/articles")
                .header("api-key", api_key.as_str())
                .json(&CreateArticleInput {
                    body_markdown: content,
                })
                .send()
                .await?;

            if !resp.status().is_success() {
                println!("Dev.to error: {:#?} {}", resp.status(), resp.text().await?);
            } else {
                println!("Post was successful");
            }
            Ok(())
        }
    })
    .await
}

async fn put(
    id: u32,
    client: Client,
    api_key: String,
    content: String,
) -> anyhow::Result<()> {
    again::retry(move || {
        let client = client.clone();
        let api_key = api_key.clone();
        let content = content.clone();
        async move {
            let resp = client
                .put(format!("https://dev.to/api/articles/{}", id).as_str())
                .header("api-key", api_key.as_str())
                .json(&CreateArticleInput {
                    body_markdown: content,
                })
                .send()
                .await?;

            if !resp.status().is_success() {
                println!("Dev.to error {:#?} {}", resp.status(), resp.text().await?);
            } else {
                println!("Update was successful");
            }
            Ok(())
        }
    })
    .await
}

async fn fetch(
    client: &Client,
    api_key: &str,
) -> anyhow::Result<Vec<Article>> {
    let resp = client
        .get("https://dev.to/api/articles/me/all?per_page=1000")
        .header("api-key", api_key)
        .send()
        .await?;

    if !resp.status().is_success() {
        bail!("Dev.to error {:#?} - bad or invalid API Key", resp.status(),);
    } else {
        Ok(resp.json().await?)
    }
}

fn valid_path(path: &PathBuf) -> bool {
    !path.is_dir()
        && path
            .extension()
            .into_iter()
            .any(|e| e == "md" || e == "markdown")
}

pub async fn run(
    api_key: String,
    args: Push,
) -> anyhow::Result<()> {
    let Push { source, dryrun } = args;
    let client = Client::new();
    let articles = fetch(&client, &api_key).await?;
    let mut hasher = Sha256::new();
    for path in WalkDir::new(source.unwrap_or_else(|| ".".into()))
        .into_iter()
        .filter_map(|e| e.ok().map(|e| e.path().to_path_buf()))
        .filter(valid_path)
    {
        let client = client.clone();
        let api_key = api_key.clone();
        let content = fs::read_to_string(&path)?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let (meta, _) = extract(name.as_ref(), &content)?;
        let status = match articles.iter().find(|a| a.title == meta.title) {
            None => UploadStatus::Posting,
            Some(remote) => {
                let differ = {
                    hasher.update(content.as_bytes());
                    let local = hasher.finalize_reset();
                    hasher.update(remote.body_markdown.as_bytes());
                    let remote = hasher.finalize_reset();
                    local != remote
                };
                if differ {
                    UploadStatus::Syncing(remote)
                } else {
                    UploadStatus::Uploaded
                }
            }
        };
        println!(
            "{}{}{}",
            meta.title.chars().take(50).collect::<String>().bold(),
            String::from(".")
                .repeat(50_usize.checked_sub(meta.title.len()).unwrap_or_default())
                .dimmed(),
            format!("[{} {}]", status, meta.publish_status()).bold(),
        );
        if !dryrun {
            match status {
                UploadStatus::Syncing(remote) => {
                    put(remote.id, client.clone(), api_key.clone(), content.clone()).await?
                }
                UploadStatus::Posting => {
                    post(client.clone(), api_key.clone(), content.clone()).await?
                }
                _ => (),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_is_published_when_expected() {
        assert_eq!(
            Frontmatter::default().publish_status(),
            PublishStatus::Draft
        );
        assert_eq!(
            Frontmatter {
                published: Some(false),
                ..Frontmatter::default()
            }
            .publish_status(),
            PublishStatus::Draft
        );
        assert_eq!(
            Frontmatter {
                published: Some(true),
                ..Frontmatter::default()
            }
            .publish_status(),
            PublishStatus::Published
        );
    }
    #[test]
    fn valid_path_isnt_dirs() {
        assert!(!valid_path(&PathBuf::from("/")))
    }

    #[test]
    fn valid_path_contains_md_ext() {
        assert!(valid_path(&PathBuf::from("/foo.md")));
    }

    #[test]
    fn valid_path_contains_markdown_ext() {
        assert!(valid_path(&PathBuf::from("/foo.markdown")));
    }

    #[test]
    fn valid_path_doesnt_contains_other_ext() {
        assert!(!valid_path(&PathBuf::from("/foo.txt")));
    }

    #[test]
    fn upload_status_impl_display() {
        fn test(_: impl fmt::Display) {}
        test(UploadStatus::Posting)
    }

    #[test]
    fn publish_status_impl_display() {
        fn test(_: impl fmt::Display) {}
        test(PublishStatus::Draft)
    }

    #[test]
    fn test_extract_fails_with_missing_frontmatter() {
        let result = extract(
            "foo.md",
            r#"
        "#,
        );
        assert!(result.is_err())
    }

    #[test]
    fn test_extract_fails_with_missing_title() {
        let result = extract(
            "foo.md",
            r#"
        --
        --
        "#,
        );
        assert!(result.is_err())
    }

    #[test]
    fn test_extract_passes_with_missing_frontmatter() -> anyhow::Result<()> {
        let (front, _) = extract(
            "foo.md",
            r#"---
            title: foo
            ---
            "#,
        )?;
        assert_eq!(
            front,
            Frontmatter {
                title: "foo".into(),
                ..Frontmatter::default()
            }
        );
        Ok(())
    }

    #[test]
    fn test_extract_validates_date() {
        let result = extract(
            "foo.md",
            r#"---
            title: foo
            date: ...
            ---
            "#,
        );
        assert!(result.is_err());
    }
}
