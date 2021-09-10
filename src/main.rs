//! An incredibly simple Markdown static site generator

use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::borrow::Cow;
use std::collections::HashMap;
use chrono::DateTime;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{Theme, ThemeSet};
use gh_emoji::Replacer;
use async_trait::async_trait;
use serde_derive::Deserialize;
use pulldown_cmark::{Parser, html, Event, Tag, CodeBlockKind};

/// Error types for this crate
#[derive(Debug)]
pub enum Error {
    /// Reading an asset to base64 failed
    ReadBase64Asset(PathBuf, std::io::Error),

    /// Failed to join with a tokio task responsible for processing a website
    WebsiteJoin(tokio::task::JoinError),

    /// Reading a config file failed
    ConfigRead(PathBuf, std::io::Error),
    
    /// Parsing a config file failed
    ConfigParse(PathBuf, toml::de::Error),

    /// Stripping the prefix from the path failed, this could only occur if
    /// the files are not correctly joined with the website's content path
    StripPrefix(PathBuf, std::path::StripPrefixError),

    /// An input had an unknown cuddly handler
    MissingHandler(PathBuf, String),

    /// Loading an additional syntax file failed
    LoadSyntax(syntect::LoadingError),

    /// Creating the generated output directory failed
    CreateOutputDir(PathBuf, std::io::Error),

    /// Reading the directory failed
    ReadDirectory(PathBuf, std::io::Error),

    /// Reading the markdown input file failed
    ReadMarkdownInput(PathBuf, std::io::Error),
    
    /// Reading the style file associated with a markdown file failed
    ReadStyle(PathBuf, PathBuf, std::io::Error),
    
    /// Reading the template HTML file associated with a markdown file failed
    ReadTemplate(PathBuf, PathBuf, std::io::Error),

    /// Parsing template TOML information from a markdown file failed
    ParseTemplateInfo(PathBuf, toml::de::Error),

    /// Writing the output HTML failed
    WriteOutput(PathBuf, std::io::Error),

    /// A markdown file did not have a `templateinfo` section
    TemplateInfoMissing(PathBuf),
}

/// Convenient `Result` wrapper around our `Error` type
pub type Result<T> = std::result::Result<T, Error>;

/// Template info included in the markdown file indicating information to be
/// used to render the HTML page
#[derive(Debug, Deserialize)]
struct TemplateInfo {
    /// Path to the CSS to use for the stylesheet for this page
    /// This is relative to `config.content_path`
    style: PathBuf,

    /// Path to the template to use for the HTML for this page
    /// This is relative to `config.content_path`
    template: PathBuf,

    /// Path to the ICO file to use as a favicon. If not specified, it will
    /// default to `favicon.ico`
    /// This is relative to `config.content_path`
    #[serde(default = "default_favicon")]
    favicon: PathBuf,

    /// Time stamp for the page
    time: DateTime<chrono::Local>,

    /// Title of the webpage
    title: String,

    /// Description of the page, also used for the OpenGraph
    description: String,
}

/// Default favicon path if one is not specified by markdown
fn default_favicon() -> PathBuf {
    PathBuf::from("favicon.ico")
}

#[derive(Default)]
struct Header;

#[derive(Debug, Deserialize)]
struct HeaderConfig {
    #[serde(default)]
    left: Vec<(String, String)>,

    #[serde(default)]
    right: Vec<(String, String)>,
}

#[async_trait]
impl Handler for Header {
    async fn handle(&self, input: &str, website: &Arc<Website>)
            -> Result<String> {
        let config: HeaderConfig = toml::from_str(input).unwrap();

        let mut output = String::new();

        output += r#"<nav class="navbar" role="navigation"><ul>"#;
        for (icon_path_or_name, href) in &config.left {
            let asset = website.load_asset(&icon_path_or_name).await
                .unwrap_or_else(|_| icon_path_or_name.clone());
            output += &format!(
                "<li><a href=\"{}\">{}</a></li>\n",
                href, asset);
        }
        for (icon_path_or_name, href) in &config.right {
            let asset = website.load_asset(&icon_path_or_name).await
                .unwrap_or_else(|_| icon_path_or_name.clone());
            output += &format!(
                "<li style=\"float:right\"><a href=\"{}\">{}</a></li>\n",
                href, asset);
        }
        output += "</nav>";

        Ok(output)
    }
}

#[derive(Default)]
struct Include;

#[derive(Debug, Deserialize)]
struct IncludeConfig {
    path: PathBuf,
}

#[async_trait]
impl Handler for Include {
    async fn handle(&self, input: &str, website: &Arc<Website>)
            -> Result<String> {
        let config: IncludeConfig = toml::from_str(input).unwrap();
        Ok(website.process_md(
            website.config.content_path.join(config.path)).await?.0)
    }
}

#[derive(Default)]
struct Index;

#[derive(Debug, Deserialize)]
struct IndexConfig {
    path: PathBuf,
}

#[async_trait]
impl Handler for Index {
    async fn handle(&self, input: &str, website: &Arc<Website>)
            -> Result<String> {
        // Get the config and the content path
        let mut config: IndexConfig = toml::from_str(input).unwrap();
        config.path = website.config.content_path.join(config.path);

        // Output HTML
        let mut output = String::new();
        output += r#"<div class="container list-posts">"#;
        output += r#"<h1 class="list-title">Blogs</h1>"#;
        output += r#"<h2 class="posts-year">2021</h2>"#;

        // Read the directory
        let mut dir = tokio::fs::read_dir(&config.path).await.map_err(|x|
            Error::ReadDirectory(config.path.clone(), x))?;
        while let Some(dirent) = dir.next_entry().await.map_err(|x|
                Error::ReadDirectory(config.path.clone(), x))? {
            let path = dirent.path();

            // Skip non-markdown files
            if path.extension().map(|x| x.eq_ignore_ascii_case("md")) !=
                    Some(true) {
                continue;
            }
            
            // Read markdown metadata
            let (_, template_info) = website.process_md(path).await?;

            output += &format!(r#"
                <article class="post-title">
                    <a href="/" class="post-link">{title}</a>
                    <div class="flex-break"></div>
                    <span class="post-date">{time}</span>
                </article>
            "#, title = template_info.title, time = template_info.time.format("%B %d, %Y"));
        }
        
        output += "</div>";

        Ok(output)
    }
}

/// A website generation session, can be shared between threads immutably
pub struct Website {
    /// Theme to use for coloring code snippits
    pub theme: Theme,

    /// Syntax set of supported syntaxes
    pub syntax_set: SyntaxSet,

    /// Emoji replacer (eg. `:smile:` to unicode smiley face)
    pub emoji_replacer: Replacer,

    /// Parsed configuration file for this website
    pub config: Config,

    /// HTML for the processed header
    pub header: String,

    /// Mapping of handler names to their Rust `Handler`s
    handlers: HashMap<String, Box<dyn Handler>>,
}

impl Website {
    /// Create a new website based on a configuration TOML file
    async fn create(config_toml: impl AsRef<Path>) -> Result<()> {
        // Read the config toml
        let config = tokio::fs::read_to_string(&config_toml).await
            .map_err(|x|
                Error::ConfigRead(config_toml.as_ref().to_path_buf(), x))?;

        // Parse the config
        let config = toml::from_str::<Config>(&config)
            .map_err(|x|
                Error::ConfigParse(config_toml.as_ref().to_path_buf(), x))?;

        // Load default syntaxes for syntax highlighting and convert it into
        // a builder so we can add custom syntaxes to it
        let mut ssb = SyntaxSet::load_defaults_newlines().into_builder();

        // Add custom syntaxes from the `syntaxes` folder
        ssb.add_from_folder("syntaxes", true).map_err(Error::LoadSyntax)?;

        // Create the website
        let mut website = Website {
            syntax_set:     ssb.build(),
            emoji_replacer: Replacer::new(),
            handlers:       HashMap::new(),
            header:         String::new(),
            theme:          ThemeSet::load_defaults()
                                .themes.remove(&config.syntax_theme).unwrap(),
            config,
        };

        website.handlers.insert("header".into(),
            Box::new(Header::default()));
        website.handlers.insert("include".into(),
            Box::new(Include::default()));
        website.handlers.insert("index".into(),
            Box::new(Index::default()));
        
        // Wrap up the website in an `Arc` for sharing between threads
        let website = Arc::new(website);

        let it = std::time::Instant::now();
        
        // Load the header file
        let header = website.process_md(website.config.content_path
            .join(&website.config.header_file)).await?;

        let website = match Arc::try_unwrap(website) {
            Ok(mut tmp) => {
                tmp.header = header.0;
                Arc::new(tmp)
            }
            Err(_) => {
                panic!();
            }
        };

        // Load the base content file
        website.process_file(website.config.content_path
            .join(&website.config.base_file)).await?;

        print!("{:?}\n", it.elapsed());

        Ok(())
    }

    /// Load an asset from disk
    async fn read_to_base64(&self, path: impl AsRef<Path>) -> Result<String> {
        // Read the image data
        let path = self.config.content_path.join(path);
        let image = tokio::fs::read(&path).await
            .map_err(|x| Error::ReadBase64Asset(path.clone(), x))?;
        
        // Convert to base64
        Ok(base64::encode(image))
    }

    /// Encapsulate an asset on disk into an HTML-embedded base64 image
    async fn load_asset(&self, path: impl AsRef<Path>) -> Result<String> {
        // Read the image data
        let path = self.config.content_path.join(path);
        let image = tokio::fs::read(&path).await
            .map_err(|x| Error::ReadBase64Asset(path.clone(), x))?;
        
        // Create image string. We don't use a format string here so that we
        // can use `encode_config_buf` without performing an extra allocation
        let mut buf = String::new();
        buf += "<img src=\"data:";
        buf += mime_guess::from_path(&path).first_raw().unwrap();
        buf += ";base64,";

        // Encode image
        base64::encode_config_buf(image, base64::STANDARD, &mut buf);

        // Finish the image string
        buf += "\" />";
        Ok(buf)
    }
    
    /// Convert the `path` markdown into HTML without encapsulating it in the
    /// templates. This just gives the raw internal HTML of the markdown
    async fn process_md(self: &Arc<Self>, path: impl AsRef<Path>)
            -> Result<(String, TemplateInfo)> {
        // Read the markdown input
        let markdown_input = tokio::fs::read_to_string(&path).await
            .map_err(|x|
                Error::ReadMarkdownInput(path.as_ref().to_path_buf(), x))?;

        // Track the current language associated with the active code block
        let mut cur_lang = None;

        // The template metadata extracted from the markdown
        let mut template_info = None;

        // String to hold the HTML output from the markdown
        let mut markdown_html = String::new();

        // Parse the markdown
        let input_md = Parser::new(&markdown_input).collect::<Vec<_>>();
        let mut extended_md = Vec::new();
        'next_event: for mut event in input_md {
            // Transform the event if needed
            match event {
                // If we see the start of a fenced code block, save the
                // language
                Event::Start(Tag::CodeBlock(
                        CodeBlockKind::Fenced(ref lang))) => {
                    // Save the current language
                    cur_lang = Some(lang.clone());

                    // Suppress templateinfo and handler stuff
                    if lang.as_ref() == "templateinfo" ||
                            lang.as_ref().starts_with("cuddly_") {
                        continue 'next_event;
                    }
                }

                // If we reach the end of the fenced code block, set that we're
                // no longer in a block
                Event::End(Tag::CodeBlock(
                        CodeBlockKind::Fenced(ref lang))) => {
                    // End the code block
                    cur_lang = None;
                    
                    // Suppress templateinfo stuff
                    // Suppress templateinfo and handler stuff
                    if lang.as_ref() == "templateinfo" ||
                            lang.as_ref().starts_with("cuddly_") {
                        continue 'next_event;
                    }
                }
            
                // If this is a text block, perform emoji transforms on it to
                // convert things like `:heart:` into their unicode equivilents
                Event::Text(ref mut text) => {
                    // If emoji replacement actually did anything, then update
                    // the old text with the new, replaced text
                    if let Cow::Owned(new_text) =
                            self.emoji_replacer.replace_all(text) {
                        *text = new_text.into();
                    }

                    // If we're currently in a code block, invoke syntect to
                    // perform syntax highlighting
                    if let Some(lang) = cur_lang.as_ref() {
                        // Attempt to figure out the syntax based on the
                        // language specified in the markdown
                        if lang.as_ref() == "templateinfo" {
                            // Save the template info
                            template_info = Some(text.to_string());
                            continue 'next_event;
                        } else if lang.as_ref().starts_with("cuddly_") {
                            // Look up the handler for this content
                            let handler = &lang.as_ref()[7..];
                            let handler = self.handlers.get(handler)
                                .ok_or_else(|| {
                                    Error::MissingHandler(
                                        path.as_ref().to_path_buf(),
                                        handler.into())
                                })?;

                            // Invoke the Rust handler
                            event = Event::Html(
                                handler.handle(&text, self).await?.into());
                        } else if let Some(syntax) =
                                self.syntax_set.find_syntax_by_token(lang) {
                            // Perform syntax highlighting by converting the
                            // string to HTML with coloring
                            let hled =
                                syntect::html::highlighted_html_for_string(
                                text, &self.syntax_set, syntax,
                                &self.theme);

                            // Update this event to no longer be a text event,
                            // but rather an HTML event
                            event = Event::Html(hled.into());
                        }
                    }
                }

                // No transforms on other events
                _ => {}
            }

            // Return the (potentially transformed) event
            extended_md.push(event);
        }

        // Conver the markdown into HTML
        html::push_html(&mut markdown_html, extended_md.into_iter());

        // Parse the template TOML into the actual `TemplateInfo` structure
        let template_info = template_info.ok_or_else(|| {
            Error::TemplateInfoMissing(path.as_ref().to_path_buf())
        })?;
        let mut template_info = toml::from_str::<TemplateInfo>(&template_info)
            .map_err(|x|
                Error::ParseTemplateInfo(path.as_ref().to_path_buf(), x))?;
        
        // Make paths relative to content path
        template_info.style =
            self.config.content_path.join(template_info.style);
        template_info.template =
            self.config.content_path.join(template_info.template);

        Ok((markdown_html, template_info))
    }

    /// Convert the `path` markdown into HTML
    async fn process_file(self: &Arc<Self>, path: impl AsRef<Path>)
            -> Result<(PathBuf, TemplateInfo)> {
        // Construct the output path for the generated HTML
        // Eg. `content/index.md` -> `output/index.html`
        let output_path = self.config.output_path.join(path.as_ref()
            .strip_prefix(&self.config.content_path)
            .map_err(|x| Error::StripPrefix(path.as_ref().to_path_buf(), x))?)
            .with_extension("html");
        
        // Convert markdown to HTML
        let (markdown_html, template_info) = self.process_md(&path).await?;

        // Create the output directories needed to create the output file
        let out_parent_dir = output_path.parent().unwrap();
        tokio::fs::create_dir_all(out_parent_dir).await
            .map_err(|x| 
                Error::CreateOutputDir(out_parent_dir.to_path_buf(), x))?;

        // Read the CSS
        let css = tokio::fs::read_to_string(&template_info.style).await
            .map_err(|x| Error::ReadStyle(path.as_ref().to_path_buf(),
                template_info.style.clone(), x))?;
        
        // Read the HTML
        let html = tokio::fs::read_to_string(&template_info.template).await
            .map_err(|x| Error::ReadTemplate(path.as_ref().to_path_buf(),
                template_info.template.clone(), x))?;
        
        // Read the favicon
        let favicon = self.read_to_base64(&template_info.favicon).await?;

        // Very high quality templating
        let html = html.replace("<<<PUT THE STYLESHEET HERE>>>", &css);
        let html = html.replace("<<<PUT THE MAIN CONTENT HERE>>>", &markdown_html);
        let html = html.replace("<<<PUT THE HEADER HERE>>>", &self.header);
        let html = html.replace("<<<PUT THE FAVICON HERE>>>", &favicon);
        let html = html.replace("<<<PUT THE TITLE HERE>>>", &template_info.title);
        let html = html.replace("<<<PUT THE DESCRIPTION HERE>>>",
            &template_info.description);

        // Write the output!
        tokio::fs::write(&output_path, html.as_bytes()).await
            .map_err(|x| Error::WriteOutput(output_path.clone(), x))?;
        
        Ok((output_path, template_info))
    }
}

#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(&self, input: &str,
        website: &Arc<Website>) -> Result<String>;
}

/// The config file for a website
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Theme to use with [`syntect::ThemeSet`]
    pub syntax_theme: String,

    /// Directory to load all content from as a base directory
    pub content_path: PathBuf,

    /// Directory to output HTML files to
    pub output_path: PathBuf,

    /// Relative to `content_path`, provides the base file where all processing
    /// starts
    pub base_file: PathBuf,

    /// Markdown file to use for the header
    pub header_file: PathBuf,
}

/// The entry point!
#[tokio::main]
async fn main() -> Result<()> {
    // Process all websites
    let mut websites = Vec::new();
    for config_toml in std::env::args().skip(1) {
        websites.push(tokio::spawn(async move {
            Website::create(config_toml).await
        }));
    }

    // Wait for all processing to complete
    for website in websites {
        website.await.map_err(Error::WebsiteJoin)??;
    }

    // Success!
    Ok(())
}

