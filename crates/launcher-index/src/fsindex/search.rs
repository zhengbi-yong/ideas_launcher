use super::*;

/// The index seen through the [`SearchTarget`] interface.
///
/// The parts that the query asks for are put together once, when the
/// candidate is looked at. Asking the index for them one by one would mean
/// a random access into a table of hundreds of megabytes for every part and
/// every term.
///
/// The path is the exception: it is only needed by the predicates that talk
/// about it, and building it means walking up the tree and joining the
/// parts. So it is built when it is asked for.

pub(super) struct IndexTarget<'a> {
    pub(in crate::fsindex) index: &'a FsIndex,
    pub(in crate::fsindex) entry: u32,
    pub(in crate::fsindex) name: &'a str,
    pub(in crate::fsindex) attributes: u32,
    pub(in crate::fsindex) depth: u32,
    pub(in crate::fsindex) alias: Option<&'a str>,
    pub(in crate::fsindex) meta: Option<FileMeta>,
    /// The characters every plain term of the query needs.
    ///
    /// They tell whether the folder of the candidate could match a term at
    /// all — and looking up the name of the folder is a random access into a
    /// table of hundreds of megabytes.
    pub(in crate::fsindex) term_masks: &'a [u64],
    /// The filter of every entry.
    pub(in crate::fsindex) filters: &'a [u64],
}

impl SearchTarget for IndexTarget<'_> {
    fn name(&self) -> &str {
        self.name
    }

    fn parent_name(&self) -> &str {
        // The name of the folder is only needed when the name of the entry
        // does not match. Looking it up costs a random access into a table
        // of hundreds of megabytes, so it is done as late as possible —
        // for a broad query most of the candidates never need it.
        let index: &FsIndex = self.index;
        if !index.folder_can_match(self.entry, self.term_masks, self.filters) {
            return "";
        }
        index.parent_name(self.entry)
    }

    fn path(&self) -> String {
        self.index.path(self.entry)
    }

    fn alias(&self) -> Option<&str> {
        self.alias
    }

    fn is_dir(&self) -> bool {
        self.attributes & usn::FILE_ATTRIBUTE_DIRECTORY != 0
    }

    fn size(&self) -> u64 {
        self.meta.map(|m| m.size).unwrap_or(0)
    }

    fn created(&self) -> u64 {
        self.meta.map(|m| m.created).unwrap_or(0)
    }

    fn modified(&self) -> u64 {
        self.meta.map(|m| m.modified).unwrap_or(0)
    }

    fn accessed(&self) -> u64 {
        self.meta.map(|m| m.accessed).unwrap_or(0)
    }

    fn attributes(&self) -> u32 {
        self.attributes
    }

    fn depth(&self) -> u32 {
        self.depth
    }
}
