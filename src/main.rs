use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use image::DynamicImage;
use image::io::Reader;
use image::imageops::{resize, FilterType};
use indicatif::{ProgressBar, ProgressStyle};
use serde_derive::{Deserialize, Serialize};
use std::ffi::OsString;
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

#[derive(Serialize, Debug)]
struct Image {
    image: String,
    thumbnail: String,
}

#[derive(Serialize)]
struct Album {
    title: String,
    thumbnail: Option<String>,
    images: Vec<Image>,
    albums: Vec<String>,
}

#[derive(Serialize)]
struct Theme {
    url: String,
}

struct Builder {
    config: Config,
    extensions: HashSet<OsString>,
    templates: tera::Tera,
}

fn resize_and_save(image: DynamicImage, width: u32, height: u32, path: PathBuf) -> Result<DynamicImage> {
    let resized = resize(&image, width, height, FilterType::Lanczos3);
    resized.save(path)?;
    Ok(image)
}

impl Builder {
    async fn new() -> Result<Self> {
        let config: Config = toml::from_str(&read_to_string(PathBuf::from(RIGAL_TOML)).await
            .context("Could not open `rigal.toml'.")?)
            .context("`rigal.toml' format seems broken.")?;

        let mut extensions: HashSet<OsString> = HashSet::new();
        let mut ext = OsString::new();
        ext.push("jpg");
        extensions.insert(ext);

        let mut templates = tera::Tera::new("_theme/templates/*.html")?;

        // We disable autoescape because we will dump a lot of path-like strings which will have to
        // be marked as "safe" by the user.
        templates.autoescape_on(vec![]);

        Ok(Builder {
            config: config,
            extensions: extensions,
            templates: templates,
        })
    }

    fn into_conversion(&self, entry: DirEntry) -> Result<Option<Conversion>> {
        let prefix = entry
            .path()
            .iter()
            .next()
            .ok_or(anyhow!("Cannot process current directory"))?;

        let path = self.config.output.join(entry.path().strip_prefix(prefix)?);

        if !path.exists() {
            return Ok(Some(Conversion { from: entry, to: path }))
        }

        if entry.metadata()?.modified()? > path.metadata()?.modified()? {
            return Ok(Some(Conversion { from: entry, to: path }))
        }

        Ok(None)
    }

    async fn process_image(&self, entry: Conversion, progress_bar: &ProgressBar) -> Result<()> {
        let mut thumbnail_path = PathBuf::from(&entry.to);
        thumbnail_path.pop();
        thumbnail_path.push("thumbnails");

        if !thumbnail_path.exists() {
            create_dir_all(&thumbnail_path).await?;
        }

        thumbnail_path.push(entry.to.file_name().unwrap());

        let image = Reader::open(entry.from.path())?.decode()?;
        let width = self.config.thumbnail.width;
        let height = self.config.thumbnail.height;

        let image = spawn_blocking(move || -> Result<DynamicImage> {
            resize_and_save(image, width, height, thumbnail_path)
        }).await??;

        if let Some(resize_config) = &self.config.resize {
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

    async fn process_images(&self) -> Result<()> {
        // Find all images that are not directories, match a supported file extension and whose output
        // either does not exist or is older than the source.
        let entries: Vec<_> = WalkDir::new(&self.config.input)
            .follow_links(true)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| self.extensions.contains(ext)))
            .map(|e| self.into_conversion(e))
            .filter_map(Result::ok)
            .filter_map(|e| e)
            .collect();

        let progress_bar = ProgressBar::new(entries.len() as u64);

        progress_bar.set_style(ProgressStyle::default_bar().template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {msg}",
        ));

        let futures: Vec<_> = entries
            .into_iter()
            .map(|e| self.process_image(e, &progress_bar))
            .collect();

        join_all(futures).await;

        Ok(())
    }

    async fn copy_static_data(&self) -> Result<()> {
        let src_root = PathBuf::from("_theme").join("static");

        if !src_root.is_dir() {
            return Ok(());
        }

        let dst_root = PathBuf::from(&self.config.output).join("static");

        for entry in WalkDir::new(&src_root) {
            let entry = entry?;
            let entry = entry.path();
            let dst = dst_root.join(entry.strip_prefix(&src_root)?);

            if entry.is_dir() {
                if !dst.exists() {
                    create_dir_all(&dst).await?;
                }
            }
            else {
                if !dst.exists() || dst.metadata()?.modified()? < entry.metadata()?.modified()? {
                    copy(entry, dst).await?;
                }
            }
        }

        Ok(())
    }

    async fn write_template(&self, entry: &DirEntry) -> Result<()> {
        let children: Vec<_> = entry
            .path()
            .read_dir()?
            .filter_map(Result::ok)
            .collect();

        let albums: Vec<_> = children
            .iter()
            .filter(|e| e.path().is_dir() && e.file_name() != "thumbnails")
            .map(|e| format!("{}/", e.path().strip_prefix(&self.config.output).unwrap().file_name().unwrap().to_string_lossy()))
            .collect();

        let images: Vec<_> = children
            .iter()
            .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| self.extensions.contains(ext)))
            .map(|e| Image {
                image: e.path().file_name().unwrap().to_string_lossy().to_string(),
                thumbnail: PathBuf::from("thumbnails").join(e.path().file_name().unwrap()).to_string_lossy().to_string(),
            })
            .collect();

        let mut static_path = PathBuf::new();

        for _ in 0..entry.path().iter().count() - 1 {
            static_path.push("..");
        }

        static_path.push("static");

        let theme = Theme {
            url: format!("{}", static_path.to_string_lossy()),
        };

        let mut context = tera::Context::new();

        context.insert("album", &Album {
            title: format!("{}", entry.file_name().to_string_lossy()),
            albums: albums,
            thumbnail: images.get(0).map_or(None, |image| Some(image.thumbnail.clone())),
            images: images,
        });

        context.insert("theme", &theme);

        let index_html = entry.path().join("index.html");
        write(index_html, self.templates.render("index.html", &context)?).await?;
        Ok(())
    }

    async fn write_templates(&self) -> Result<()> {
        fn must_skip(entry: &DirEntry) -> bool {
            entry.file_type().is_file() ||
                (entry.file_type().is_dir() && (entry.file_name() == "thumbnails" || entry.file_name() == "static"))
        }

        for entry in WalkDir::new(&self.config.output)
            .into_iter()
            .filter_entry(|e| !must_skip(e)) {
            let entry = entry?;

            self.write_template(&entry).await?;
        }

        Ok(())
    }
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

async fn build() -> Result<()> {
    let builder = Builder::new().await?;

    builder.process_images().await?;
    builder.copy_static_data().await?;
    builder.write_templates().await
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
