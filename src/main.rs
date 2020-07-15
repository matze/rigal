use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use image::DynamicImage;
use image::io::Reader;
use image::imageops::{resize, FilterType};
use indicatif::{ProgressBar, ProgressStyle};
use serde_derive::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::collections::HashSet;
use structopt::StructOpt;
use tera;
use tokio::fs::{copy, create_dir_all, read_to_string, write};
use tokio::task::spawn_blocking;
use walkdir::{DirEntry, WalkDir};

static RIGAL_TOML: &str = "rigal.toml";

#[derive(StructOpt)]
#[structopt(name = "rigal", about = "Static photo gallery generator")]
enum Commands {
    #[structopt(about = "Build static gallery")]
    Build,

    #[structopt(about = "Create new rigal.toml config")]
    New,
}

#[derive(Serialize, Deserialize)]
struct ThumbnailSize {
    width: u32,
    height: u32,
}

#[derive(Serialize, Deserialize)]
struct Resize {
    width: u32,
    height: u32,
}

#[derive(Serialize, Deserialize)]
struct Config {
    input: PathBuf,
    output: PathBuf,
    thumbnail: ThumbnailSize,
    resize: Option<Resize>,
}

#[derive(Debug)]
struct Conversion {
    from: DirEntry,
    to: PathBuf,
}

#[derive(Serialize)]
struct Album {
    title: String,
    images: Vec<String>,
    albums: Vec<String>,
}

async fn create_config() -> Result<()> {
    let config = Config {
        input: PathBuf::from("input"),
        output: PathBuf::from("_build"),
        thumbnail: ThumbnailSize {
            width: 450,
            height: 300,
        },
        resize: None,
    };

    write(PathBuf::from(RIGAL_TOML), toml::to_string(&config)?).await?;
    println!("Wrote {}.", RIGAL_TOML);

    Ok(())
}

fn resize_and_save(image: DynamicImage, width: u32, height: u32, path: PathBuf) -> Result<DynamicImage> {
    let resized = resize(&image, width, height, FilterType::Lanczos3);
    resized.save(path)?;
    Ok(image)
}

async fn process(entry: Conversion, config: &Config, progress_bar: &ProgressBar) -> Result<()> {
    let mut thumbnail_path = PathBuf::from(&entry.to);
    thumbnail_path.pop();
    thumbnail_path.push("thumbnails");

    if !thumbnail_path.exists() {
        create_dir_all(&thumbnail_path).await?;
    }

    thumbnail_path.push(entry.to.file_name().unwrap());

    let image = Reader::open(entry.from.path())?.decode()?;
    let width = config.thumbnail.width;
    let height = config.thumbnail.height;

    let image = spawn_blocking(move || -> Result<DynamicImage> {
        resize_and_save(image, width, height, thumbnail_path)
    }).await??;

    if let Some(resize_config) = &config.resize {
        // User asks for resizing the source images, so lets do that.
        let width = resize_config.width;
        let height  = resize_config.height;

        spawn_blocking(move || -> Result<DynamicImage> {
            resize_and_save(image, width, height, entry.to)
        }).await??;
    }
    else {
        // No resizing required, just copy the source file.
        copy(entry.from.path(), &entry.to).await?;
    }

    progress_bar.inc(1);

    Ok(())
}

async fn build() -> Result<()> {
    let config: Config = toml::from_str(&read_to_string(PathBuf::from(RIGAL_TOML)).await
        .context("Could not open `rigal.toml'.")?)
        .context("`rigal.toml' format seems broken.")?;

    let mut extensions = HashSet::new();
    extensions.insert(OsStr::new("jpg"));

    let into_conversion = |entry: DirEntry| -> Result<Option<Conversion>> {
        let prefix = entry
            .path()
            .iter()
            .next()
            .ok_or(anyhow!("Cannot process current directory"))?;

        let path = config.output
            .clone()
            .join(entry.path().strip_prefix(prefix)?);

        if !path.exists() {
            return Ok(Some(Conversion { from: entry, to: path }))
        }

        if entry.metadata()?.modified()? > path.metadata()?.modified()? {
            return Ok(Some(Conversion { from: entry, to: path }))
        }

        Ok(None)
    };

    // Find all images that are not directories, match a supported file extension and whose output
    // either does not exist or is older than the source.
    let entries: Vec<_> = WalkDir::new(&config.input)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| extensions.contains(ext)))
        .map(|e| into_conversion(e))
        .filter_map(Result::ok)
        .filter_map(|e| e)
        .collect();

    let progress_bar = ProgressBar::new(entries.len() as u64);

    progress_bar.set_style(ProgressStyle::default_bar().template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {msg}",
    ));

    let futures: Vec<_> = entries.into_iter().map(|e| process(e, &config, &progress_bar)).collect();
    join_all(futures).await;

    let templates = tera::Tera::new("_theme/templates/*.html")?;

    for entry in WalkDir::new(&config.output) {
        let entry = entry?;

        if entry.file_type().is_dir() && entry.file_name() != "thumbnails" {
            let children: Vec<_> = entry
                .path()
                .read_dir()?
                .filter_map(Result::ok)
                .collect();

            let albums: Vec<_> = children
                .iter()
                .filter(|e| e.path().is_dir() && e.file_name() != "thumbnails")
                .map(|e| format!("{}/", e.file_name().to_string_lossy()))
                .collect();

            let images: Vec<_> = children
                .iter()
                .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| extensions.contains(ext)))
                .map(|e| format!("{}", e.file_name().to_string_lossy()))
                .collect();

            let mut context = tera::Context::new();

            context.insert("album", &Album {
                title: format!("{}", entry.file_name().to_string_lossy()),
                albums: albums,
                images: images,
            });

            let index_html = entry.path().join("index.html");
            write(index_html, templates.render("index.html", &context)?).await?;
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let commands = Commands::from_args();

    match commands {
        Commands::Build => {
            build().await?;
        }
        Commands::New => {
            create_config().await?;
        }
    }

    Ok(())
}
