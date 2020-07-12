use anyhow::{anyhow, Context, Result};
use image::io::Reader;
use image::imageops::{resize, FilterType};
use indicatif::{ProgressBar, ProgressStyle};
use serde_derive::Deserialize;
use std::ffi::OsStr;
use std::fs::{copy, create_dir_all, read_to_string};
use std::path::PathBuf;
use std::collections::HashSet;
use structopt::StructOpt;
use walkdir::{DirEntry, WalkDir};

#[derive(StructOpt)]
#[structopt(name = "rigal", about = "Static photo gallery generator")]
struct Options {
    #[structopt(parse(from_os_str))]
    input: PathBuf,

    #[structopt(parse(from_os_str), default_value = "_build")]
    output: PathBuf,
}

#[derive(Deserialize)]
struct ThumbnailSize {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Resize {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Config {
    thumbnail: ThumbnailSize,
    resize: Option<Resize>,
}

#[derive(Debug)]
struct Conversion {
    from: DirEntry,
    to: PathBuf,
}

fn main() -> Result<()> {
    let options = Options::from_args();

    let config: Config = toml::from_str(&read_to_string(PathBuf::from("rigal.toml"))
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

        let path = options.output
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
    let entries: Vec<_> = WalkDir::new(&options.input)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().map_or(false, |ext| extensions.contains(ext)))
        .filter(|e| !e.file_type().is_dir())
        .map(|e| into_conversion(e))
        .filter_map(Result::ok)
        .filter_map(|e| e)
        .collect();

    let progress_bar = ProgressBar::new(entries.len() as u64);

    progress_bar.set_style(ProgressStyle::default_bar().template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {msg}",
    ));

    for entry in entries {
        progress_bar.set_message(&format!("Processing {:?}", entry.from.path()));

        let image = Reader::open(entry.from.path())?.decode()?;
        let thumbnail = resize(&image, config.thumbnail.width, config.thumbnail.height, FilterType::Lanczos3);

        let mut thumbnail_path = PathBuf::from(&entry.to);
        thumbnail_path.pop();
        thumbnail_path.push("thumbnails");

        if !thumbnail_path.exists() {
            create_dir_all(&thumbnail_path)?;
        }

        thumbnail_path.push(entry.to.file_name().unwrap());
        thumbnail.save(thumbnail_path)?;

        if let Some(resize_config) = &config.resize {
            let resized = resize(&image, resize_config.width, resize_config.height, FilterType::Lanczos3);
            resized.save(&entry.to)?;
        }
        else {
            copy(entry.from.path(), &entry.to)?;
        }

        progress_bar.inc(1);
    }

    Ok(())
}
