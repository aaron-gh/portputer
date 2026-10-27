//! Files: browsing the server's folders, uploading to it, downloading from it,
//! and the files on its clipboard. Each operation runs on a connection of its
//! own (a Noise handshake with `transfer` in the hello), so a large file
//! doesn't hold up keys on the control connection. File data goes in data
//! frames between JSON messages.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::Error;
use crate::transport::{Frame, MAX_DATA, Reader, Writer};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferRequest {
    /// A folder's contents; an empty path is the home folder.
    List { path: String },
    Download { path: String },
    /// Followed by `size` bytes of data frames. With `clipboard`, the file
    /// joins the files of that copy on the server's clipboard; without, it's
    /// saved in Downloads.
    Upload {
        name: String,
        size: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clipboard: Option<u64>,
    },
    /// The files on the server's clipboard, each as `file_start`, data and
    /// `file_end`, then `done`.
    ClipboardFiles,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferReply {
    Listing { path: String, parent: Option<String>, entries: Vec<Entry>, places: Vec<Place> },
    FileStart { name: String, size: u64 },
    FileEnd,
    Done { path: String },
    Error { message: String },
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Where uploads are saved. PORTPUTER_DOWNLOADS replaces it, for tests.
pub fn downloads() -> PathBuf {
    std::env::var_os("PORTPUTER_DOWNLOADS").map(PathBuf::from).unwrap_or_else(|| home().join("Downloads"))
}

/// Where files copied to this computer's clipboard from another device are
/// kept.
pub fn clipboard_dir(batch: u64) -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Caches/Portputer/Clipboard")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("Portputer").join("Clipboard")
    } else {
        std::env::temp_dir().join("portputer-clipboard")
    };
    base.join(format!("{batch:x}"))
}

fn places() -> Vec<Place> {
    let h = home();
    let mut out = vec![Place { name: "Home".into(), path: h.display().to_string() }];
    for name in ["Desktop", "Documents", "Downloads"] {
        let p = h.join(name);
        if p.is_dir() {
            out.push(Place { name: name.into(), path: p.display().to_string() });
        }
    }
    if cfg!(target_os = "macos") {
        if let Ok(volumes) = std::fs::read_dir("/Volumes") {
            for v in volumes.flatten() {
                out.push(Place { name: v.file_name().to_string_lossy().into(), path: v.path().display().to_string() });
            }
        }
    }
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if Path::new(&root).is_dir() {
                out.push(Place { name: format!("Drive {}", letter as char), path: root });
            }
        }
    }
    out
}

/// A file name as received: its last component only, so it can't climb out
/// of the folder it's saved in.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        "file".into()
    } else {
        base.to_string()
    }
}

/// A path in `dir` for `name` that isn't taken: "name 2.ext" and so on.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = safe_name(name);
    let first = dir.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (2..).map(|n| dir.join(format!("{stem} {n}{ext}"))).find(|p| !p.exists()).unwrap()
}

fn list(path: &str) -> Result<TransferReply, String> {
    let dir = if path.is_empty() { home() } else { PathBuf::from(path) };
    let read = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let meta = std::fs::metadata(e.path()).ok()?;
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: e.path().display().to_string(),
                dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
            })
        })
        .collect();
    entries.sort_by(|a, b| (!a.dir, a.name.to_lowercase()).cmp(&(!b.dir, b.name.to_lowercase())));
    Ok(TransferReply::Listing {
        path: dir.display().to_string(),
        parent: dir.parent().map(|p| p.display().to_string()),
        entries,
        places: places(),
    })
}

async fn send_file(writer: &Writer, path: &Path) -> Result<(), Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    writer.send(&TransferReply::FileStart { name, size }).await?;
    let mut buf = vec![0u8; MAX_DATA];
    let mut sent = 0u64;
    while sent < size {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.send_data(&buf[..n]).await?;
        sent += n as u64;
    }
    writer.send(&TransferReply::FileEnd).await
}

/// Receives `size` bytes of data frames into a file.
async fn receive_into<T: serde::de::DeserializeOwned>(
    reader: &mut Reader,
    path: &Path,
    size: u64,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(".portputer-part");
    let partial = PathBuf::from(partial);
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut received = 0u64;
    while received < size {
        match reader.recv_frame::<T>().await? {
            Frame::Data(data) => {
                file.write_all(&data).await?;
                received += data.len() as u64;
                progress(received, size);
            }
            Frame::Message(_) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(Error::Protocol("the transfer stopped early".into()));
            }
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&partial, path).await?;
    Ok(())
}

/// The clipboard copy that uploads are joining, and its files so far.
pub(crate) type ClipboardBatch = std::sync::Mutex<(u64, Vec<PathBuf>)>;

/// The server's side of a transfer connection.
pub(crate) async fn serve(
    mut reader: Reader,
    writer: Writer,
    batch: &ClipboardBatch,
    own_clipboard: &std::sync::atomic::AtomicI64,
) -> Result<(), Error> {
    loop {
        let request = match reader.recv_frame::<TransferRequest>().await {
            Ok(Frame::Message(m)) => m,
            Ok(Frame::Data(_)) => return Err(Error::Protocol("data without an upload".into())),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let result: Result<(), Error> = async {
            match request {
                TransferRequest::List { path } => {
                    let reply = tokio::task::spawn_blocking(move || list(&path)).await.map_err(|e| Error::Protocol(e.to_string()))?;
                    match reply {
                        Ok(r) => writer.send(&r).await,
                        Err(message) => writer.send(&TransferReply::Error { message }).await,
                    }
                }
                TransferRequest::Download { path } => {
                    let path = PathBuf::from(path);
                    if path.is_dir() {
                        return writer.send(&TransferReply::Error { message: "Folders can't be downloaded yet.".into() }).await;
                    }
                    send_file(&writer, &path).await?;
                    writer.send(&TransferReply::Done { path: path.display().to_string() }).await
                }
                TransferRequest::Upload { name, size, clipboard } => {
                    let dir = match clipboard {
                        Some(id) => clipboard_dir(id),
                        None => downloads(),
                    };
                    tokio::fs::create_dir_all(&dir).await?;
                    let path = unique_path(&dir, &name);
                    receive_into::<TransferRequest>(&mut reader, &path, size, &|_, _| {}).await?;
                    if let Some(id) = clipboard {
                        let files = {
                            let mut b = batch.lock().unwrap();
                            if b.0 != id {
                                *b = (id, vec![]);
                            }
                            b.1.push(path.clone());
                            b.1.clone()
                        };
                        let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
                        if crate::clipboard::write_files(&refs).is_ok() {
                            own_clipboard.store(crate::clipboard::change_count(), std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    writer.send(&TransferReply::Done { path: path.display().to_string() }).await
                }
                TransferRequest::ClipboardFiles => {
                    let files = match tokio::task::spawn_blocking(crate::clipboard::read).await.ok().flatten() {
                        Some(crate::clipboard::Clip::Files(f)) => f,
                        _ => vec![],
                    };
                    for f in files.iter().filter(|f| f.is_file()) {
                        send_file(&writer, f).await?;
                    }
                    writer.send(&TransferReply::Done { path: String::new() }).await
                }
            }
        }
        .await;
        if let Err(e) = result {
            match e {
                Error::Io(io) => writer.send(&TransferReply::Error { message: io.to_string() }).await?,
                other => return Err(other),
            }
        }
    }
}

/// The client's side, on a connection already through the handshake and
/// the welcome.
pub(crate) struct TransferClient {
    pub reader: Reader,
    pub writer: Writer,
}

impl TransferClient {
    async fn reply(&mut self) -> Result<TransferReply, Error> {
        match self.reader.recv_frame::<TransferReply>().await? {
            Frame::Message(TransferReply::Error { message }) => Err(Error::Refused(message)),
            Frame::Message(m) => Ok(m),
            Frame::Data(_) => Err(Error::Protocol("unexpected data".into())),
        }
    }

    pub async fn list(&mut self, path: &str) -> Result<TransferReply, Error> {
        self.writer.send(&TransferRequest::List { path: path.into() }).await?;
        self.reply().await
    }

    /// Receives files until `done`, into `dest`, returning where each went.
    async fn receive_files(&mut self, dest: &Path, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<Vec<PathBuf>, Error> {
        let mut saved = vec![];
        loop {
            match self.reply().await? {
                TransferReply::FileStart { name, size } => {
                    let path = unique_path(dest, &name);
                    receive_into::<TransferReply>(&mut self.reader, &path, size, progress).await?;
                    match self.reply().await? {
                        TransferReply::FileEnd => saved.push(path),
                        _ => return Err(Error::Protocol("no end of file".into())),
                    }
                }
                TransferReply::Done { .. } => return Ok(saved),
                _ => return Err(Error::Protocol("unexpected reply".into())),
            }
        }
    }

    pub async fn download(&mut self, path: &str, dest: &Path, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<PathBuf, Error> {
        self.writer.send(&TransferRequest::Download { path: path.into() }).await?;
        self.receive_files(dest, progress).await?.into_iter().next().ok_or_else(|| Error::Protocol("no file".into()))
    }

    pub async fn clipboard_files(&mut self, dest: &Path, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<Vec<PathBuf>, Error> {
        self.writer.send(&TransferRequest::ClipboardFiles).await?;
        self.receive_files(dest, progress).await
    }

    pub async fn upload(&mut self, path: &Path, name: Option<&str>, clipboard: Option<u64>, progress: &(dyn Fn(u64, u64) + Send + Sync)) -> Result<String, Error> {
        let mut file = tokio::fs::File::open(path).await?;
        let size = file.metadata().await?.len();
        let name = name.map(String::from).unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into()));
        self.writer.send(&TransferRequest::Upload { name, size, clipboard }).await?;
        let mut buf = vec![0u8; MAX_DATA];
        let mut sent = 0u64;
        while sent < size {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            self.writer.send_data(&buf[..n]).await?;
            sent += n as u64;
            progress(sent, size);
        }
        match self.reply().await? {
            TransferReply::Done { path } => Ok(path),
            _ => Err(Error::Protocol("unexpected reply".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\x\\y.txt"), "y.txt");
        assert_eq!(safe_name(".."), "file");
        let dir = std::env::temp_dir().join(format!("portputer-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a 2.txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
