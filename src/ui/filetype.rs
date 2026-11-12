//! What kind of file something is, from its name: the icon, color and the
//! human label ("PNG image") shown in the Kind column.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Category {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Document,
    Pdf,
    Spreadsheet,
    Presentation,
    Font,
    DiskImage,
    Executable,
    Other,
}

impl Category {
    pub fn icon(self) -> &'static str {
        match self {
            Category::Folder => "folder",
            Category::Image => "file-image",
            Category::Video => "file-video",
            Category::Audio => "file-audio",
            Category::Archive => "file-archive",
            Category::Code => "file-code",
            Category::Text => "file-text",
            Category::Document => "file-text",
            Category::Pdf => "file-pdf",
            Category::Spreadsheet => "file-sheet",
            Category::Presentation => "file-slides",
            Category::Font => "file-font",
            Category::DiskImage => "disc",
            Category::Executable => "cpu",
            Category::Other => "file",
        }
    }

    /// Hue in degrees for the icon.
    pub fn hue(self) -> f32 {
        match self {
            Category::Folder => 222.0,
            Category::Image => 328.0,
            Category::Video => 270.0,
            Category::Audio => 24.0,
            Category::Archive => 44.0,
            Category::Code => 150.0,
            Category::Text => 210.0,
            Category::Document => 205.0,
            Category::Pdf => 355.0,
            Category::Spreadsheet => 130.0,
            Category::Presentation => 16.0,
            Category::Font => 290.0,
            Category::DiskImage => 190.0,
            Category::Executable => 170.0,
            Category::Other => 220.0,
        }
    }
}

/// Lowercased extension of a file name, without the dot. Hidden files like
/// `.bashrc` have none.
pub fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i + 1..],
    }
}

/// (category, label) for a regular file.
pub fn classify(name: &str, executable: bool) -> (Category, &'static str) {
    let ext = extension(name);
    let mut buf = [0u8; 16];
    let lower = if ext.len() <= buf.len() && ext.is_ascii() {
        let b = &mut buf[..ext.len()];
        b.copy_from_slice(ext.as_bytes());
        b.make_ascii_lowercase();
        std::str::from_utf8(b).unwrap_or("")
    } else {
        ""
    };
    if let Some(found) = by_extension(lower) {
        return found;
    }
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return (Category::Code, "Makefile"),
        "Dockerfile" | "Containerfile" => return (Category::Code, "Dockerfile"),
        "PKGBUILD" => return (Category::Code, "PKGBUILD"),
        "LICENSE" | "COPYING" | "README" | "AUTHORS" | "CHANGELOG" | "NEWS" | "TODO" => {
            return (Category::Text, "Text");
        }
        _ => {}
    }
    if name.starts_with(".") && lower.is_empty() {
        // Dotfiles are almost always configuration.
        return (Category::Code, "Configuration");
    }
    if executable {
        return (Category::Executable, "Program");
    }
    (Category::Other, "File")
}

fn by_extension(ext: &str) -> Option<(Category, &'static str)> {
    use Category::*;
    Some(match ext {
        "" => return None,
        // Images
        "png" => (Image, "PNG image"),
        "jpg" | "jpeg" | "jfif" => (Image, "JPEG image"),
        "gif" => (Image, "GIF image"),
        "webp" => (Image, "WebP image"),
        "avif" => (Image, "AVIF image"),
        "heic" | "heif" => (Image, "HEIF image"),
        "bmp" => (Image, "Bitmap image"),
        "ico" => (Image, "Icon"),
        "tif" | "tiff" => (Image, "TIFF image"),
        "svg" | "svgz" => (Image, "SVG image"),
        "qoi" => (Image, "QOI image"),
        "psd" => (Image, "Photoshop image"),
        "xcf" => (Image, "GIMP image"),
        "kra" => (Image, "Krita image"),
        "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" => (Image, "Raw photo"),
        "exr" | "hdr" => (Image, "HDR image"),
        // Video
        "mp4" | "m4v" => (Video, "MP4 video"),
        "mkv" => (Video, "Matroska video"),
        "webm" => (Video, "WebM video"),
        "mov" => (Video, "QuickTime video"),
        "avi" => (Video, "AVI video"),
        "wmv" => (Video, "Windows Media video"),
        "flv" => (Video, "Flash video"),
        "mpg" | "mpeg" | "m2ts" => (Video, "MPEG video"),
        "3gp" => (Video, "3GP video"),
        "ogv" => (Video, "Ogg video"),
        // Audio
        "mp3" => (Audio, "MP3 audio"),
        "flac" => (Audio, "FLAC audio"),
        "wav" => (Audio, "WAV audio"),
        "ogg" | "oga" => (Audio, "Ogg audio"),
        "opus" => (Audio, "Opus audio"),
        "m4a" | "aac" => (Audio, "AAC audio"),
        "wma" => (Audio, "Windows Media audio"),
        "aiff" | "aif" => (Audio, "AIFF audio"),
        "mid" | "midi" => (Audio, "MIDI"),
        // Archives
        "zip" => (Archive, "ZIP archive"),
        "tar" => (Archive, "Tar archive"),
        "gz" | "tgz" => (Archive, "Gzip archive"),
        "xz" | "txz" => (Archive, "XZ archive"),
        "bz2" | "tbz2" => (Archive, "Bzip2 archive"),
        "zst" | "tzst" => (Archive, "Zstandard archive"),
        "7z" => (Archive, "7-Zip archive"),
        "rar" => (Archive, "RAR archive"),
        "lz" | "lzma" | "lz4" => (Archive, "Compressed file"),
        "deb" => (Archive, "Debian package"),
        "rpm" => (Archive, "RPM package"),
        "jar" => (Archive, "Java archive"),
        "apk" => (Archive, "Android package"),
        "crate" => (Archive, "Rust crate"),
        "whl" => (Archive, "Python wheel"),
        // Disk images
        "iso" => (DiskImage, "Disc image"),
        "img" => (DiskImage, "Disk image"),
        "qcow2" | "vdi" | "vmdk" | "vhd" | "vhdx" => (DiskImage, "Virtual disk"),
        "dmg" => (DiskImage, "Apple disk image"),
        "appimage" => (Executable, "AppImage"),
        // Documents
        "pdf" => (Pdf, "PDF document"),
        "doc" | "docx" => (Document, "Word document"),
        "odt" => (Document, "OpenDocument text"),
        "rtf" => (Document, "Rich text"),
        "epub" => (Document, "EPUB book"),
        "mobi" | "azw3" => (Document, "E-book"),
        "djvu" => (Document, "DjVu document"),
        "xls" | "xlsx" => (Spreadsheet, "Excel spreadsheet"),
        "ods" => (Spreadsheet, "OpenDocument spreadsheet"),
        "csv" => (Spreadsheet, "CSV table"),
        "tsv" => (Spreadsheet, "TSV table"),
        "ppt" | "pptx" => (Presentation, "PowerPoint"),
        "odp" => (Presentation, "OpenDocument slides"),
        "key" => (Presentation, "Keynote"),
        // Fonts
        "ttf" => (Font, "TrueType font"),
        "otf" => (Font, "OpenType font"),
        "woff" | "woff2" => (Font, "Web font"),
        // Text
        "txt" | "text" => (Text, "Text"),
        "md" | "markdown" => (Text, "Markdown"),
        "rst" => (Text, "reStructuredText"),
        "org" => (Text, "Org document"),
        "log" => (Text, "Log"),
        "tex" => (Text, "LaTeX"),
        "srt" | "vtt" | "ass" => (Text, "Subtitles"),
        // Code and config
        "rs" => (Code, "Rust"),
        "c" => (Code, "C"),
        "h" => (Code, "C header"),
        "cc" | "cpp" | "cxx" | "c++" => (Code, "C++"),
        "hpp" | "hh" | "hxx" => (Code, "C++ header"),
        "cs" => (Code, "C#"),
        "go" => (Code, "Go"),
        "py" | "pyw" => (Code, "Python"),
        "ipynb" => (Code, "Jupyter notebook"),
        "js" | "mjs" | "cjs" => (Code, "JavaScript"),
        "jsx" => (Code, "React JSX"),
        "ts" | "mts" => (Code, "TypeScript"),
        "tsx" => (Code, "React TSX"),
        "java" => (Code, "Java"),
        "kt" | "kts" => (Code, "Kotlin"),
        "swift" => (Code, "Swift"),
        "rb" => (Code, "Ruby"),
        "php" => (Code, "PHP"),
        "lua" => (Code, "Lua"),
        "zig" => (Code, "Zig"),
        "dart" => (Code, "Dart"),
        "ex" | "exs" => (Code, "Elixir"),
        "erl" => (Code, "Erlang"),
        "hs" => (Code, "Haskell"),
        "ml" | "mli" => (Code, "OCaml"),
        "scala" => (Code, "Scala"),
        "clj" => (Code, "Clojure"),
        "r" => (Code, "R"),
        "jl" => (Code, "Julia"),
        "nim" => (Code, "Nim"),
        "v" | "sv" | "vhdl" => (Code, "HDL"),
        "asm" | "s" => (Code, "Assembly"),
        "cu" | "cuh" => (Code, "CUDA"),
        "glsl" | "vert" | "frag" | "wgsl" | "hlsl" => (Code, "Shader"),
        "sh" | "bash" | "zsh" | "fish" => (Code, "Shell script"),
        "ps1" => (Code, "PowerShell"),
        "bat" | "cmd" => (Code, "Batch file"),
        "html" | "htm" => (Code, "HTML"),
        "css" | "scss" | "sass" | "less" => (Code, "Stylesheet"),
        "vue" | "svelte" => (Code, "Component"),
        "json" | "jsonc" | "json5" => (Code, "JSON"),
        "yaml" | "yml" => (Code, "YAML"),
        "toml" => (Code, "TOML"),
        "xml" => (Code, "XML"),
        "ini" | "cfg" | "conf" | "config" | "properties" | "env" => (Code, "Configuration"),
        "lock" => (Code, "Lockfile"),
        "sql" => (Code, "SQL"),
        "proto" => (Code, "Protocol Buffers"),
        "graphql" | "gql" => (Code, "GraphQL"),
        "cmake" => (Code, "CMake"),
        "mk" => (Code, "Makefile"),
        "nix" => (Code, "Nix"),
        "diff" | "patch" => (Code, "Patch"),
        "desktop" => (Code, "Desktop entry"),
        "service" | "timer" | "socket" | "mount" => (Code, "systemd unit"),
        "qml" => (Code, "QML"),
        "gd" => (Code, "GDScript"),
        "tscn" | "tres" => (Code, "Godot resource"),
        "kicad_pcb" | "kicad_sch" | "kicad_pro" => (Code, "KiCad"),
        // Binaries
        "exe" | "msi" => (Executable, "Windows program"),
        "so" => (Executable, "Shared library"),
        "o" | "a" => (Executable, "Object file"),
        "bin" => (Executable, "Binary"),
        "wasm" => (Executable, "WebAssembly"),
        "torrent" => (Other, "Torrent"),
        "db" | "sqlite" | "sqlite3" => (Other, "Database"),
        "bak" | "old" | "orig" | "swp" | "tmp" => (Other, "Backup"),
        "part" | "crdownload" => (Other, "Partial download"),
        "kdbx" => (Other, "KeePass database"),
        "gpg" | "asc" | "sig" | "pem" | "crt" | "pub" => (Code, "Key"),
        _ => return None,
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(classify("photo.JPG", false).0, Category::Image);
        assert_eq!(classify("main.rs", false).1, "Rust");
        assert_eq!(classify(".bashrc", false).0, Category::Code);
        assert_eq!(classify("run", true).0, Category::Executable);
        assert_eq!(classify("notes", false).0, Category::Other);
        assert_eq!(classify("Makefile", false).0, Category::Code);
        assert_eq!(extension(".hidden"), "");
        assert_eq!(extension("a.tar.gz"), "gz");
    }
}

#[cfg(test)]
mod speed {
    #[test]
    #[ignore]
    fn classify_speed() {
        let names: Vec<String> = (0..1_000_000)
            .map(|i| {
                let e = [
                    "jpg", "png", "txt", "rs", "pdf", "mp3", "mkv", "zip", "md", "json", "toml",
                    "c", "h", "py", "docx", "xyz",
                ][i % 16];
                format!("Report_{i:05} {i}.{e}")
            })
            .collect();
        let t = std::time::Instant::now();
        let mut n = 0;
        for s in &names {
            n += super::classify(s, false).1.len();
        }
        eprintln!(
            "classify: {:.1} ns/name ({n})",
            t.elapsed().as_nanos() as f64 / 1e6
        );
    }
}
