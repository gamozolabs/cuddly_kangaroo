//! An incredibly simple Markdown static site generator

use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::borrow::Cow;
use chrono::DateTime;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{Theme, ThemeSet};
use gh_emoji::Replacer;
use serde_derive::Deserialize;
use pulldown_cmark::{Parser, html, Event, Tag, CodeBlockKind};

/// Directory where all of the input content is. This should have all the
/// assets, templates, etc.
///
/// All lookups through helper functions like `load_asset` will be relative to
/// this directory.
const CONTENT_PATH: &str = "content";

/// Directory to place all generated content into
const OUTPUT_PATH: &str = "output";

/// Error types for this crate
#[derive(Debug)]
enum Error {
    /// Reading an asset to base64 failed
    ReadBase64Asset(PathBuf, std::io::Error),

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
type Result<T> = std::result::Result<T, Error>;

/// Load an asset from disk
async fn read_to_base64<P: AsRef<Path>>(path: P) -> Result<String> {
    // Read the image data
    let path = Path::new(CONTENT_PATH).join(path);
    let image = tokio::fs::read(&path).await
        .map_err(|x| Error::ReadBase64Asset(path.clone(), x))?;
    
    // Convert to base64
    Ok(base64::encode(image))
}

/// Encapsulate an asset on disk into an HTML-embedded base64 image
async fn load_asset<P: AsRef<Path>>(path: P) -> Result<String> {
    // Read the image data
    let path = Path::new(CONTENT_PATH).join(path);
    let image = tokio::fs::read(&path).await
        .map_err(|x| Error::ReadBase64Asset(path.clone(), x))?;
    
    // Create image string. We don't use a format string here so that we can
    // use `encode_config_buf` without performing an extra allocation
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

/// Template info included in the markdown file indicating information to be
/// used to render the HTML page
#[derive(Debug, Deserialize)]
struct TemplateInfo {
    /// Path to the CSS to use for the stylesheet for this page
    /// This is relative to `CONTENT_PATH`
    style: PathBuf,

    /// Path to the template to use for the HTML for this page
    /// This is relative to `CONTENT_PATH`
    template: PathBuf,

    /// Path to the ICO file to use as a favicon. If not specified, it will
    /// default to `favicon.ico`
    /// This is relative to `CONTENT_PATH`
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

/// Convert the `path` markdown into HTML
async fn process_file(path: PathBuf, ctxt: Arc<Context>)
        -> Result<(PathBuf, TemplateInfo)> {
    // Construct the output path for the generated HTML
    // Eg. `content/index.md` -> `output/index.html`
    let output_path = Path::new(OUTPUT_PATH).join(path
        .strip_prefix(CONTENT_PATH).unwrap()).with_extension("html");

    // Read the markdown input
    let markdown_input = tokio::fs::read_to_string(&path).await
        .map_err(|x| Error::ReadMarkdownInput(path.to_path_buf(), x))?;

    // Track the current language associated with the active code block
    let mut cur_lang = None;

    // The template metadata extracted from the markdown
    let mut template_info = None;

    // String to hold the HTML output from the markdown
    let mut markdown_html = String::new();

    // Parse the markdown and convert it to HTML
    html::push_html(&mut markdown_html,
            Parser::new(&markdown_input).filter_map(|mut event| {
        // Transform the event if needed
        match event {
            // If we see the start of a fenced code block, save the language
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref lang))) => {
                cur_lang = Some(lang.clone());

                // Suppress templateinfo stuff
                if lang.as_ref() == "templateinfo" {
                    return None;
                }
            }

            // If we reach the end of the fenced code block, set that we're no
            // longer in a block
            Event::End(Tag::CodeBlock(CodeBlockKind::Fenced(ref lang))) => {
                cur_lang = None;
                
                // Suppress templateinfo stuff
                if lang.as_ref() == "templateinfo" {
                    return None;
                }
            }
        
            // If this is a text block, perform emoji transforms on it to
            // convert things like `:heart:` into their unicode equivilents
            Event::Text(ref mut text) => {
                // If emoji replacement actually did anything, then update the
                // old text with the new, replaced text
                if let Cow::Owned(new_text) =
                        ctxt.emoji_replacer.replace_all(text) {
                    *text = new_text.into();
                }

                // If we're currently in a code block, invoke syntect to
                // perform syntax highlighting
                if let Some(lang) = cur_lang.as_ref() {
                    // Attempt to figure out the syntax based on the language
                    // specified in the markdown
                    if lang.as_ref() == "templateinfo" {
                        // Save the template info
                        template_info = Some(text.to_string());
                        return None;
                    } else if let Some(syntax) =
                            ctxt.syntax_set.find_syntax_by_token(lang) {
                        // Perform syntax highlighting by converting the
                        // string to HTML with coloring
                        let hled = syntect::html::highlighted_html_for_string(
                            text, &ctxt.syntax_set, syntax,
                            &ctxt.theme);

                        // Update this event to no longer be a text event, but
                        // rather an HTML event
                        event = Event::Html(hled.into());
                    }
                }
            }

            // No transforms on other events
            _ => {}
        }

        // Return the (potentially transformed) event
        Some(event)
    }));

    // Parse the template TOML into the actual `TemplateInfo` structure
    let template_info = template_info.ok_or_else(|| {
        Error::TemplateInfoMissing(path.to_path_buf())
    })?;
    let mut template_info = dbg!(toml::from_str::<TemplateInfo>(&template_info)
        .map_err(|x| Error::ParseTemplateInfo(path.to_path_buf(), x))?);

    // Create the output directories needed to create the output file
    let out_parent_dir = output_path.parent().unwrap();
    tokio::fs::create_dir_all(out_parent_dir).await
        .map_err(|x| Error::CreateOutputDir(out_parent_dir.to_path_buf(), x))?;

    // Read the CSS
    template_info.style = Path::new(CONTENT_PATH).join(template_info.style);
    let css = tokio::fs::read_to_string(&template_info.style).await
        .map_err(|x| Error::ReadStyle(path.to_path_buf(),
            template_info.style.clone(), x))?;
    
    // Read the HTML
    template_info.template =
        Path::new(CONTENT_PATH).join(template_info.template);
    let html = tokio::fs::read_to_string(&template_info.template).await
        .map_err(|x| Error::ReadTemplate(path.to_path_buf(),
            template_info.template.clone(), x))?;
    
    // Read the favicon
    let favicon = read_to_base64(&template_info.favicon).await?;

    // Compute the navbar
    let navs = [
        ("Home", "/"), ("Blog", "/blog"), ("About", "/about"),
    ];
    let social = [
        ("feather/twitter.svg", "https://twitter.com/gamozolabs"),
        ("feather/twitch.svg", "https://twitch.tv/gamozo"),
        ("feather/youtube.svg", "https://youtube.com/gamozolabs"),
        ("feather/github.svg", "https://github.com/gamozolabs"),
    ];

    let mut header = String::new();
    header += r#"
            <nav class="navbar" role="navigation">
                <ul>
"#;
    for (name, path) in navs {
        header += &format!("<li><a href=\"{}\">{}</a></li>\n", path, name);
    }
    for (icon, path) in social {
        let asset = load_asset(icon).await?;
        header += &format!(
            "<li style=\"float:right\"><a href=\"{}\">{}</a></li>\n",
            path, asset);
    }
    header += r#"
                </ul>
            </nav>
"#;

    let mut list = String::new();
    list += "<div class=\"list-posts\"><h1 class=\"list-title\">Blogs</h1>\n";

    // Get the directory listing
    let in_parent_dir = path.parent().unwrap();
    let mut dir = tokio::fs::read_dir(&in_parent_dir).await
        .map_err(|x| Error::ReadDirectory(in_parent_dir.to_path_buf(), x))?;

    // Go through each file in the directory
    while let Some(ent) = dir.next_entry().await.map_err(|x|
            Error::ReadDirectory(in_parent_dir.to_path_buf(), x))? {
        // Get the file metadata
        let metadata = ent.metadata().await
            .map_err(|x|
                Error::ReadDirectory(in_parent_dir.to_path_buf(), x))?;

        // Get the file path
        let path = ent.path();

        if metadata.is_file() && path.extension()
                .map(|x| x.eq_ignore_ascii_case("md")) == Some(true) {
            list += &format!("<article class=\"post-title\"><a href=\"asdf\" class=\"post-link\">{:?}</a><div class=\"flex-break\"></div>\n<span class=\"post-date\">October 23, 2020</span></article>", path);
        }
    }

    list += "</div>";

    // Very high quality templating
    let html = html.replace("<<<PUT THE STYLESHEET HERE>>>", &css);
    let html = html.replace("<<<PUT THE MAIN CONTENT HERE>>>", &markdown_html);
    let html = html.replace("<<<PUT THE HEADER HERE>>>", &header);
    let html = html.replace("<<<PUT THE FAVICON HERE>>>", &favicon);
    let html = html.replace("<<<PUT THE TITLE HERE>>>", &template_info.title);
    let html = html.replace("<<<PUT THE DESCRIPTION HERE>>>",
        &template_info.description);
    let html = html.replace("<<<PUT THE LIST HERE>>>", &list);

    // Write the output!
    tokio::fs::write(&output_path, html.as_bytes()).await
        .map_err(|x| Error::WriteOutput(output_path.clone(), x))?;
    
    Ok((output_path, template_info))
}

/// Convert all markdown in `path` to HTML, if no `index.md` is present at
/// `path`, one will be created for you which holds the file listing of the
/// other files in this folder.
async fn load_directory<P: AsRef<Path>>(path: P, ctxt: Arc<Context>)
        -> Result<()> {
    // List of tasks we've created
    let mut tasks = Vec::new();

    // List of directories to search
    let mut paths = vec![path.as_ref().to_path_buf()];

    // Go through each path to explore
    while let Some(path) = paths.pop() {
        // Get the directory listing
        let mut dir = tokio::fs::read_dir(&path).await
            .map_err(|x| Error::ReadDirectory(path.clone(), x))?;

        // Go through each file in the directory
        while let Some(ent) = dir.next_entry().await
                .map_err(|x| Error::ReadDirectory(path.clone(), x))? {
            // Get the file metadata
            let metadata = ent.metadata().await
                .map_err(|x| Error::ReadDirectory(path.clone(), x))?;

            // Get the file path
            let path = ent.path();

            // If this is a directory, recurse into it
            if metadata.is_dir() {
                paths.push(path);
            } else if metadata.is_file() && path.extension()
                    .map(|x| x.eq_ignore_ascii_case("md")) == Some(true) {
                // If this is a file and it has an `md` extension, process it
                let ctxt = ctxt.clone();
                tasks.push(tokio::spawn(async move {
                    process_file(path, ctxt).await
                }));
            }
        }
    }

    // Wait for all the tasks to complete
    let mut output_html = Vec::new();
    for task in tasks {
        output_html.push(task.await.expect("Failed to join task")?);
    }

    // Now that we've processed all markdown files, now we can make directories
    let mut parents = std::collections::HashMap::new();
    for (path, template) in &output_html {
        parents.entry(path.parent().unwrap()).or_insert_with(|| std::collections::HashMap::new())
            .insert(path.file_name().unwrap().to_str().unwrap(), template);
    }

    for (parent, children) in &parents {
        if children.contains_key("index.html") {
            continue;
        }

        print!("NEEDS INDEX {:?}\n", parent);
    }

    Ok(())
}

/// Misc data that we want to pass to multiple threads.
///
/// This is really just for data which is fetched once and we don't want to
/// keep duplicating the work for every worker.
struct Context {
    /// Theme to use for coloring code snippits
    theme: Theme,

    /// Syntax set of supported syntaxes
    syntax_set: SyntaxSet,
    
    /// Emoji replacer (eg. `:smile:` to unicode smiley face)
    emoji_replacer: Replacer,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load default syntaxes
    let mut ssb = SyntaxSet::load_defaults_newlines().into_builder();

    // Add custom syntaxes
    ssb.add_from_folder("syntaxes", true).map_err(Error::LoadSyntax)?;

    // Create initialize-once context
    let ctxt = Arc::new(Context {
        // Pick the theme we want to use
        theme: ThemeSet::load_defaults().themes
            .remove("InspiredGitHub").unwrap(),
    
        // Get list of supported syntaxes
        syntax_set: ssb.build(),

        // Initialize emoji state
        emoji_replacer: Replacer::new(),
    });

    // Read the root level content directory
    load_directory(CONTENT_PATH, ctxt).await?;

    Ok(())
}

