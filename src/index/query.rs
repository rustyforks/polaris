use anyhow::*;
use diesel;
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types;
#[cfg(feature = "profile-index")]
use flame;
use std::path::Path;

use crate::db::{directories, songs, DB};
use crate::index::*;
use crate::vfs::VFSSource;

no_arg_sql_function!(
	random,
	sql_types::Integer,
	"Represents the SQL RANDOM() function"
);

#[cfg_attr(feature = "profile-index", flame)]
pub fn virtualize_song(vfs: &VFS, mut song: Song) -> Option<Song> {
	song.path = match vfs.real_to_virtual(Path::new(&song.path)) {
		Ok(p) => p.to_string_lossy().into_owned(),
		_ => return None,
	};
	if let Some(artwork_path) = song.artwork {
		song.artwork = match vfs.real_to_virtual(Path::new(&artwork_path)) {
			Ok(p) => Some(p.to_string_lossy().into_owned()),
			_ => None,
		};
	}
	Some(song)
}

#[cfg_attr(feature = "profile-index", flame)]
fn virtualize_directory(vfs: &VFS, mut directory: Directory) -> Option<Directory> {
	directory.path = match vfs.real_to_virtual(Path::new(&directory.path)) {
		Ok(p) => p.to_string_lossy().into_owned(),
		_ => return None,
	};
	if let Some(artwork_path) = directory.artwork {
		directory.artwork = match vfs.real_to_virtual(Path::new(&artwork_path)) {
			Ok(p) => Some(p.to_string_lossy().into_owned()),
			_ => None,
		};
	}
	Some(directory)
}

pub fn browse<P>(db: &DB, virtual_path: P) -> Result<Vec<CollectionFile>>
where
	P: AsRef<Path>,
{
	let mut output = Vec::new();
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;

	if virtual_path.as_ref().components().count() == 0 {
		// Browse top-level
		let real_directories: Vec<Directory> = directories::table
			.filter(directories::parent.is_null())
			.load(&connection)?;
		let virtual_directories = real_directories
			.into_iter()
			.filter_map(|s| virtualize_directory(&vfs, s));
		output.extend(virtual_directories.map(CollectionFile::Directory));
	} else {
		// Browse sub-directory
		let real_path = vfs.virtual_to_real(virtual_path)?;
		let real_path_string = real_path.as_path().to_string_lossy().into_owned();

		let real_directories: Vec<Directory> = directories::table
			.filter(directories::parent.eq(&real_path_string))
			.order(sql::<sql_types::Bool>("path COLLATE NOCASE ASC"))
			.load(&connection)?;
		let virtual_directories = real_directories
			.into_iter()
			.filter_map(|s| virtualize_directory(&vfs, s));
		output.extend(virtual_directories.map(CollectionFile::Directory));

		let real_songs: Vec<Song> = songs::table
			.filter(songs::parent.eq(&real_path_string))
			.order(sql::<sql_types::Bool>("path COLLATE NOCASE ASC"))
			.load(&connection)?;
		let virtual_songs = real_songs
			.into_iter()
			.filter_map(|s| virtualize_song(&vfs, s));
		output.extend(virtual_songs.map(CollectionFile::Song));
	}

	Ok(output)
}

pub fn flatten<P>(db: &DB, virtual_path: P) -> Result<Vec<Song>>
where
	P: AsRef<Path>,
{
	use self::songs::dsl::*;
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;

	let real_songs: Vec<Song> = if virtual_path.as_ref().parent() != None {
		let real_path = vfs.virtual_to_real(virtual_path)?;
		let like_path = real_path.as_path().to_string_lossy().into_owned() + "%";
		songs
			.filter(path.like(&like_path))
			.order(path)
			.load(&connection)?
	} else {
		songs.order(path).load(&connection)?
	};

	let virtual_songs = real_songs
		.into_iter()
		.filter_map(|s| virtualize_song(&vfs, s));
	Ok(virtual_songs.collect::<Vec<_>>())
}

pub fn get_random_albums(db: &DB, count: i64) -> Result<Vec<Directory>> {
	use self::directories::dsl::*;
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;
	let real_directories = directories
		.filter(album.is_not_null())
		.limit(count)
		.order(random)
		.load(&connection)?;
	let virtual_directories = real_directories
		.into_iter()
		.filter_map(|s| virtualize_directory(&vfs, s));
	Ok(virtual_directories.collect::<Vec<_>>())
}

pub fn get_recent_albums(db: &DB, count: i64) -> Result<Vec<Directory>> {
	use self::directories::dsl::*;
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;
	let real_directories: Vec<Directory> = directories
		.filter(album.is_not_null())
		.order(date_added.desc())
		.limit(count)
		.load(&connection)?;
	let virtual_directories = real_directories
		.into_iter()
		.filter_map(|s| virtualize_directory(&vfs, s));
	Ok(virtual_directories.collect::<Vec<_>>())
}

pub fn search(db: &DB, query: &str) -> Result<Vec<CollectionFile>> {
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;
	let like_test = format!("%{}%", query);
	let mut output = Vec::new();

	// Find dirs with matching path and parent not matching
	{
		use self::directories::dsl::*;
		let real_directories: Vec<Directory> = directories
			.filter(path.like(&like_test))
			.filter(parent.not_like(&like_test))
			.load(&connection)?;

		let virtual_directories = real_directories
			.into_iter()
			.filter_map(|s| virtualize_directory(&vfs, s));

		output.extend(virtual_directories.map(CollectionFile::Directory));
	}

	// Find songs with matching title/album/artist and non-matching parent
	{
		use self::songs::dsl::*;
		let real_songs: Vec<Song> = songs
			.filter(
				path.like(&like_test)
					.or(title.like(&like_test))
					.or(album.like(&like_test))
					.or(artist.like(&like_test))
					.or(album_artist.like(&like_test)),
			)
			.filter(parent.not_like(&like_test))
			.load(&connection)?;

		let virtual_songs = real_songs
			.into_iter()
			.filter_map(|s| virtualize_song(&vfs, s));

		output.extend(virtual_songs.map(CollectionFile::Song));
	}

	Ok(output)
}

pub fn get_song(db: &DB, virtual_path: &Path) -> Result<Song> {
	let vfs = db.get_vfs()?;
	let connection = db.connect()?;
	let real_path = vfs.virtual_to_real(virtual_path)?;
	let real_path_string = real_path.as_path().to_string_lossy();

	use self::songs::dsl::*;
	let real_song: Song = songs
		.filter(path.eq(real_path_string))
		.get_result(&connection)?;

	match virtualize_song(&vfs, real_song) {
		Some(s) => Ok(s),
		_ => bail!("Missing VFS mapping"),
	}
}
