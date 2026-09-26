//! The Recycle Bin. Deleting a file or folder in File Explorer or on
//! the desktop moves it to `/$Recycle.Bin/<user>/` instead of erasing
//! it, so it can be put back. `$index.txt` there remembers where each
//! item came from, one `name<TAB>original path` line per item.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Error, Info};

pub const ROOT: &str = "/$Recycle.Bin";
const INDEX: &str = "$index.txt";

/// A user's bin folder.
pub fn folder(user: &str) -> String {
    super::join(ROOT, user)
}

/// Whether a path is a user's bin folder or inside it.
pub fn contains(user: &str, path: &str) -> bool {
    let bin = folder(user);
    path.len() >= bin.len()
        && super::same_name(&path[..bin.len()], &bin)
        && (path.len() == bin.len() || path.as_bytes()[bin.len()] == b'/')
}

fn ensure(user: &str) -> String {
    let _ = super::create_dir(ROOT);
    let bin = folder(user);
    let _ = super::create_dir(&bin);
    bin
}

fn read_index(user: &str) -> Vec<(String, String)> {
    let data = super::read(&super::join(&folder(user), INDEX)).unwrap_or_default();
    let text = String::from_utf8_lossy(&data);
    text.lines()
        .filter_map(|l| {
            let (name, path) = l.split_once('\t')?;
            Some((String::from(name), String::from(path)))
        })
        .collect()
}

fn write_index(user: &str, index: &[(String, String)]) {
    let mut text = String::new();
    for (name, path) in index {
        text.push_str(name);
        text.push('\t');
        text.push_str(path);
        text.push('\n');
    }
    let _ = super::write(&super::join(&folder(user), INDEX), text.as_bytes());
}

/// Move a file or folder to the bin.
pub fn recycle(user: &str, path: &str) -> Result<(), Error> {
    if contains(user, path) {
        return super::remove(path);
    }
    let bin = ensure(user);
    let name = super::file_name(path);
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !super::is_dir(path) => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let stored = super::unique_name(&bin, base, ext);
    super::move_path(path, &super::join(&bin, &stored))?;
    let mut index = read_index(user);
    index.push((stored, String::from(path)));
    write_index(user, &index);
    Ok(())
}

/// What is in the bin.
pub fn items(user: &str) -> Vec<Info> {
    super::list(&folder(user)).unwrap_or_default()
}

pub fn is_empty(user: &str) -> bool {
    items(user).is_empty()
}

/// Where an item in the bin was deleted from.
pub fn original(user: &str, name: &str) -> Option<String> {
    read_index(user)
        .into_iter()
        .find(|(n, _)| super::same_name(n, name))
        .map(|(_, p)| p)
}

/// Put an item back where it was deleted from. Returns its path.
pub fn restore(user: &str, name: &str) -> Result<String, Error> {
    let bin = folder(user);
    let from = super::join(&bin, name);
    let mut index = read_index(user);
    let at = index.iter().position(|(n, _)| super::same_name(n, name));
    let target = match at {
        Some(i) => index[i].1.clone(),
        // not in the index: back to the user's desktop
        None => super::join(&super::join(&super::home(user), "Desktop"), name),
    };
    // make the folders it was in again, if they are gone
    let dir = super::parent(&target);
    let mut acc = String::new();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        acc.push('/');
        acc.push_str(part);
        let _ = super::create_dir(&acc);
    }
    let mut to = target.clone();
    if super::exists(&to) {
        let file = super::file_name(&target);
        let (base, ext) = match file.rfind('.') {
            Some(i) if i > 0 && !super::is_dir(&from) => (&file[..i], &file[i..]),
            _ => (file, ""),
        };
        to = super::join(&dir, &super::unique_name(&dir, base, ext));
    }
    super::move_path(&from, &to)?;
    if let Some(i) = at {
        index.remove(i);
        write_index(user, &index);
    }
    Ok(to)
}

/// Delete an item in the bin for good.
pub fn purge(user: &str, name: &str) -> Result<(), Error> {
    super::remove(&super::join(&folder(user), name))?;
    let mut index = read_index(user);
    index.retain(|(n, _)| !super::same_name(n, name));
    write_index(user, &index);
    Ok(())
}

/// Delete everything in the bin for good.
pub fn empty(user: &str) -> Result<(), Error> {
    let bin = folder(user);
    if super::exists(&bin) {
        super::remove(&bin)?;
    }
    ensure(user);
    Ok(())
}
