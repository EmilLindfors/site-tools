//! Reading citation keys out of a local Zotero library.
//!
//! Zotero 8 added a native `citationKey` field, and Better BibTeX migrated its own store
//! into it — on this machine on 2026-07-27, leaving `better-bibtex.sqlite` behind as
//! `better-bibtex.migrated`. So there is one database to read rather than two, and the
//! key lives in `itemData` next to the title and the DOI instead of in a plugin's cache.
//!
//! The database is opened read-only and immutable. Zotero holds a write lock on it while
//! it is running, and immutable mode is what lets a build read the library without
//! asking the author to close their reference manager first. It also means a build can
//! never write to it, which is the right guarantee for something a build script runs.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::bib::Reference;

/// One item, as far as this cares.
pub struct Item {
    pub citekey: String,
    pub item_type: String,
    pub title: String,
    pub year: String,
    pub journal: Option<String>,
    pub volume: Option<String>,
    pub number: Option<String>,
    pub pages: Option<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    /// (given, family), in the order the item lists them.
    pub authors: Vec<(String, String)>,
}

pub struct Library {
    conn: Connection,
}

impl Library {
    /// Open the Zotero library at `zotero.sqlite` under `dir`.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let path = dir.join("zotero.sqlite");
        if !path.exists() {
            return Err(format!("No Zotero library at {}", path.display()));
        }

        // `immutable=1` skips the locking protocol entirely, so a running Zotero does
        // not block the read. It is only sound because this never writes.
        let uri = format!("file:{}?mode=ro&immutable=1", path.display());
        let conn = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| format!("Open {}: {e}", path.display()))?;

        // Fail here rather than on the first lookup: a library from before Zotero 8 has
        // no citationKey field at all, and "citekey not found" would be a misleading way
        // to say so.
        let has_field: bool = conn
            .query_row(
                "select exists(select 1 from fields where fieldName = 'citationKey')",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Read {}: {e}", path.display()))?;
        if !has_field {
            return Err(format!(
                "{} has no citationKey field. Zotero 8 introduced it and Better BibTeX \
                 migrates into it; upgrade Zotero, or give the key a DOI in the post's \
                 [extra.bib] so it resolves against crossref instead",
                path.display()
            ));
        }

        Ok(Self { conn })
    }

    /// Every citekey in the library, with the item's title, sorted.
    pub fn citekeys(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "select ck.value, coalesce(t.value, '')
                   from itemData dk
                   join itemDataValues ck on ck.valueID = dk.valueID
                   join fields fk on fk.fieldID = dk.fieldID and fk.fieldName = 'citationKey'
                   left join itemData dt on dt.itemID = dk.itemID
                        and dt.fieldID = (select fieldID from fields where fieldName = 'title')
                   left join itemDataValues t on t.valueID = dt.valueID
                  where dk.itemID not in (select itemID from deletedItems)
                  order by ck.value collate nocase",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Look one citekey up.
    pub fn lookup(&self, citekey: &str) -> Result<Item, String> {
        let item_id: i64 = self
            .conn
            .query_row(
                "select dk.itemID
                   from itemData dk
                   join itemDataValues ck on ck.valueID = dk.valueID
                   join fields fk on fk.fieldID = dk.fieldID and fk.fieldName = 'citationKey'
                  where ck.value = ?1
                    and dk.itemID not in (select itemID from deletedItems)
                  limit 1",
                [citekey],
                |row| row.get(0),
            )
            .map_err(|_| format!("@{citekey} is not in the Zotero library"))?;

        let item_type: String = self
            .conn
            .query_row(
                "select t.typeName from items i
                   join itemTypes t on t.itemTypeID = i.itemTypeID
                  where i.itemID = ?1",
                [item_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        let mut stmt = self
            .conn
            .prepare(
                "select f.fieldName, v.value from itemData d
                   join itemDataValues v on v.valueID = d.valueID
                   join fields f on f.fieldID = d.fieldID
                  where d.itemID = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut fields: Vec<(String, String)> = stmt
            .query_map([item_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        fields.sort();

        let get = |name: &str| {
            fields
                .iter()
                .find(|(f, _)| f == name)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };

        let mut stmt = self
            .conn
            .prepare(
                "select cr.firstName, cr.lastName from itemCreators ic
                   join creators cr on cr.creatorID = ic.creatorID
                   join creatorTypes ct on ct.creatorTypeID = ic.creatorTypeID
                  where ic.itemID = ?1 and ct.creatorType = 'author'
                  order by ic.orderIndex",
            )
            .map_err(|e| e.to_string())?;
        let authors: Vec<(String, String)> = stmt
            .query_map([item_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;

        Ok(Item {
            citekey: citekey.to_string(),
            item_type,
            title: get("title").unwrap_or_default(),
            year: year_of(get("date").as_deref().unwrap_or_default()),
            journal: get("publicationTitle"),
            volume: get("volume"),
            number: get("issue"),
            pages: get("pages"),
            publisher: get("publisher"),
            isbn: get("ISBN"),
            doi: get("DOI"),
            url: get("url"),
            authors,
        })
    }
}

/// The year out of a Zotero date field.
///
/// Zotero stores a normalised prefix and the user's original string in one value:
/// `2017-00-00 2017`, `2021-03-00 March 2021`, or just `1942`. The first four digits are
/// the year in every shape it takes.
fn year_of(date: &str) -> String {
    let mut run = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            if run.len() == 4 {
                return run;
            }
        } else if !run.is_empty() {
            run.clear();
        }
    }
    String::new()
}

/// Where Zotero keeps its data by default, unless `ZOTERO_DATA_DIR` says otherwise.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZOTERO_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Zotero")
}

impl Item {
    /// The reference record, under the anchor the post cites it by.
    pub fn to_reference(&self, anchor: &str) -> Reference {
        let families: Vec<String> = self
            .authors
            .iter()
            .map(|(_, family)| family.clone())
            .filter(|s| !s.is_empty())
            .collect();

        Reference {
            key: anchor.to_string(),
            kind: kind_of(&self.item_type),
            author: crate::sources::apa_authors_from_parts(&self.authors),
            title: self.title.clone(),
            year: self.year.clone(),
            journal: self.journal.clone(),
            volume: self.volume.clone(),
            number: self.number.clone(),
            pages: self.pages.clone(),
            // Same rule as the crossref side: `bib.reference` prints a publisher for a
            // book alone, and Zotero records one for journal articles too.
            publisher: (kind_of(&self.item_type) == "book")
                .then(|| self.publisher.clone())
                .flatten(),
            booktitle: None,
            school: None,
            isbn: self.isbn.clone(),
            url: self
                .url
                .clone()
                .filter(|u| !crate::sources::is_doi_resolver(u, self.doi.as_deref().unwrap_or(""))),
            doi: self.doi.clone(),
            families,
        }
    }
}

/// Zotero's item types, mapped onto the ones `bib.reference` branches on.
fn kind_of(item_type: &str) -> String {
    match item_type {
        "journalArticle" | "preprint" | "magazineArticle" | "newspaperArticle" => "article",
        "book" | "bookSection" => "book",
        "conferencePaper" => "inproceedings",
        "thesis" => "phdthesis",
        _ => "misc",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zotero writes the normalised date and the user's original into one field.
    #[test]
    fn the_year_comes_out_of_every_date_shape() {
        assert_eq!(year_of("2017-00-00 2017"), "2017");
        assert_eq!(year_of("2021-03-00 March 2021"), "2021");
        assert_eq!(year_of("1942"), "1942");
        assert_eq!(year_of("2020-05-14 2020-05-14"), "2020");
        assert_eq!(year_of(""), "");
        assert_eq!(year_of("n.d."), "");
    }

    /// A run of digits shorter than four is not a year, and must not be glued to the
    /// next run to make one.
    #[test]
    fn short_digit_runs_are_not_years() {
        assert_eq!(year_of("14 May, 2020"), "2020");
        assert_eq!(year_of("1-2-3"), "");
    }

    #[test]
    fn zotero_types_map_onto_the_component_branches() {
        assert_eq!(kind_of("journalArticle"), "article");
        assert_eq!(kind_of("conferencePaper"), "inproceedings");
        assert_eq!(kind_of("thesis"), "phdthesis");
        assert_eq!(kind_of("book"), "book");
        assert_eq!(kind_of("blogPost"), "misc");
    }

    #[test]
    fn the_data_dir_is_overridable() {
        // Set for the duration of this test only; the default path is machine-specific
        // and not worth asserting on.
        unsafe { std::env::set_var("ZOTERO_DATA_DIR", "/tmp/zotero-test") };
        assert_eq!(default_data_dir(), PathBuf::from("/tmp/zotero-test"));
        unsafe { std::env::remove_var("ZOTERO_DATA_DIR") };
    }

    /// Opening something that is not a Zotero library says so, rather than failing later
    /// on a lookup.
    #[test]
    fn a_missing_library_is_reported_at_open() {
        let err = match Library::open(Path::new("/definitely/not/here")) {
            Err(e) => e,
            Ok(_) => panic!("opened a library that is not there"),
        };
        assert!(err.contains("No Zotero library"), "{err}");
    }
}
