use crate::{
    cursor,
    filesystem::{RootDir, blocked},
};
use std::{
    collections::{BinaryHeap, VecDeque},
    ffi::OsString,
    path::PathBuf,
};

pub(crate) const MAX_DIR_ENTRIES: usize = 100_000;
pub(crate) struct WalkEntry {
    pub real: PathBuf,
    pub logical: PathBuf,
    pub components: Vec<String>,
    pub file_type: cap_std::fs::FileType,
    pub depth: usize,
    pub skipped_before: usize,
}
pub(crate) enum WalkEvent {
    Entry(WalkEntry),
    Inaccessible,
    TailSkipped(usize),
}
struct Frame {
    real: PathBuf,
    logical: PathBuf,
    components: Vec<String>,
    names: VecDeque<(String, OsString, bool)>,
    lower: Option<String>,
    more: bool,
}
pub(crate) struct Walker<'a> {
    root: &'a RootDir,
    root_id: &'a str,
    frames: Vec<Frame>,
    pending: Option<Frame>,
    max_depth: usize,
    resume: Option<Vec<String>>,
    include_resume: bool,
    window: usize,
    skipped: usize,
    #[cfg(test)]
    reads: usize,
}
impl<'a> Walker<'a> {
    pub(crate) fn new(
        root: &'a RootDir,
        root_id: &'a str,
        base_real: PathBuf,
        base_logical: PathBuf,
        max_depth: usize,
        resume_after: Option<Vec<String>>,
        include_resume_entry: bool,
    ) -> Self {
        Self {
            root,
            root_id,
            frames: vec![],
            pending: Some(Frame {
                real: base_real,
                logical: base_logical,
                components: vec![],
                names: VecDeque::new(),
                lower: None,
                more: true,
            }),
            max_depth,
            resume: resume_after,
            include_resume: include_resume_entry,
            window: MAX_DIR_ENTRIES,
            skipped: 0,
            #[cfg(test)]
            reads: 0,
        }
    }
    fn fill(&mut self, frame: &mut Frame) -> std::io::Result<()> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let children = self.root.dir.read_dir(&frame.real)?;
        let mut heap = BinaryHeap::new();
        let mut more = false;
        for child in children {
            let name = child?.file_name();
            let key = name.to_string_lossy().into_owned();
            if frame.lower.as_ref().is_some_and(|lower| key <= *lower)
                || blocked(&frame.logical.join(&name))
            {
                continue;
            }
            // Rebuild only the window at the resume point, keeping its ancestor
            // directory so that we can descend into it without reading siblings.
            if let Some(resume) = &self.resume
                && resume.starts_with(&frame.components)
                && let Some(bound) = resume.get(frame.components.len())
                && key < *bound
            {
                continue;
            }
            heap.push((key, name));
            if heap.len() > self.window {
                heap.pop();
                more = true;
            }
        }
        frame.names = heap
            .into_sorted_vec()
            .into_iter()
            .map(|(key, name)| {
                let fits = cursor::walk_child_fits(self.root_id, &frame.components, &key);
                (key, name, fits)
            })
            .collect();
        frame.more = more;
        Ok(())
    }
}
impl Iterator for Walker<'_> {
    type Item = WalkEvent;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(mut frame) = self.pending.take() {
                if self.fill(&mut frame).is_err() {
                    return Some(WalkEvent::Inaccessible);
                }
                self.frames.push(frame);
            }
            let Some(mut frame) = self.frames.pop() else {
                return (self.skipped > 0)
                    .then(|| WalkEvent::TailSkipped(std::mem::take(&mut self.skipped)));
            };
            let Some((key, name, fits)) = frame.names.pop_front() else {
                if frame.more {
                    if self.fill(&mut frame).is_err() {
                        return Some(WalkEvent::Inaccessible);
                    }
                    self.frames.push(frame);
                }
                continue;
            };
            frame.lower = Some(key.clone());
            let real = frame.real.join(&name);
            let logical = frame.logical.join(&name);
            let mut components = frame.components.clone();
            components.push(key);
            self.frames.push(frame);
            let before = self.resume.as_ref().is_some_and(|r| components <= *r);
            let ancestor = self
                .resume
                .as_ref()
                .is_some_and(|r| r.starts_with(&components));
            if before && !ancestor {
                continue;
            }
            if !fits {
                if !before {
                    self.skipped += 1;
                }
                continue;
            }
            let file_type = match self.root.dir.symlink_metadata(&real) {
                Ok(meta) => meta.file_type(),
                Err(_) => return Some(WalkEvent::Inaccessible),
            };
            let depth = components.len();
            if file_type.is_dir() && !file_type.is_symlink() && depth < self.max_depth {
                self.pending = Some(Frame {
                    real: real.clone(),
                    logical: logical.clone(),
                    components: components.clone(),
                    names: VecDeque::new(),
                    lower: None,
                    more: true,
                });
            }
            if before && !(self.include_resume && self.resume.as_ref() == Some(&components)) {
                continue;
            }
            return Some(WalkEvent::Entry(WalkEntry {
                real,
                logical,
                components,
                file_type,
                depth,
                skipped_before: std::mem::take(&mut self.skipped),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Root;
    #[test]
    fn windowed_preorder_and_every_resume_cover_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        for path in [
            "a/1.txt", "a/2.txt", "b/1.txt", "c.txt", "d.txt", "e.txt", ".env", ".git/x",
        ] {
            let p = tmp.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", tmp.path().join("link")).unwrap();
        let root = RootDir::open(&Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap();
        let make = |resume| {
            let mut w = Walker::new(&root, "work", ".".into(), "".into(), 5, resume, false);
            w.window = 2;
            w
        };
        let collect = |w: Walker<'_>| {
            w.filter_map(|e| match e {
                WalkEvent::Entry(e) => Some(e.components),
                _ => None,
            })
            .collect::<Vec<_>>()
        };
        let full = collect(make(None));
        let names = full.iter().map(|p| p.join("/")).collect::<Vec<_>>();
        assert_eq!(
            &names[..8],
            [
                "a", "a/1.txt", "a/2.txt", "b", "b/1.txt", "c.txt", "d.txt", "e.txt"
            ]
        );
        assert!(
            names
                .iter()
                .all(|p| !p.starts_with('.') && !p.starts_with("link/"))
        );
        for (i, position) in full.iter().enumerate() {
            assert_eq!(collect(make(Some(position.clone()))), full[i + 1..]);
        }
        let mut w = make(Some(vec!["b".into(), "1.txt".into()]));
        let tail = w.by_ref().count();
        assert!(tail >= 3);
        // Root windows and b, but never the skipped subtree a.
        assert_eq!(w.reads, if cfg!(unix) { 4 } else { 3 });
        let resumed = collect(Walker::new(
            &root,
            "work",
            ".".into(),
            "".into(),
            5,
            Some(vec!["a".into(), "1.txt".into()]),
            true,
        ));
        assert_eq!(resumed[0], ["a", "1.txt"]);
        std::fs::remove_file(tmp.path().join("b/1.txt")).unwrap();
        let deleted = full.iter().position(|p| p == &["b", "1.txt"]).unwrap();
        assert_eq!(
            collect(make(Some(vec!["b".into(), "1.txt".into()]))),
            full[deleted + 1..]
        );
    }
    #[test]
    #[cfg(not(windows))]
    fn exact_positions_skip_children_once_across_windows_and_every_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let root = RootDir::open(&Root {
            id: "work".into(),
            path: tmp.path().into(),
            nickname: None,
        })
        .unwrap();
        let name = "d".repeat(255);
        let parents = vec![name.clone(); 5];
        let limit = (1..=255)
            .find(|length| !cursor::walk_child_fits("work", &parents, &"f".repeat(*length)))
            .unwrap()
            - 1;
        let fitting = "f".repeat(limit);
        let omitted = "g".repeat(limit + 1);
        let mut dir = root.dir.try_clone().unwrap();
        for _ in 0..5 {
            dir.create_dir(&name).unwrap();
            dir = dir.open_dir(&name).unwrap();
        }
        dir.create_dir(&name).unwrap();
        let inner = dir.open_dir(&name).unwrap();
        inner.write("hidden.txt", b"needle").unwrap();
        for file in ["deep.txt", fitting.as_str(), omitted.as_str()] {
            dir.write(file, b"needle").unwrap();
        }
        let make = |resume, include, depth| {
            let mut walker =
                Walker::new(&root, "work", ".".into(), "".into(), depth, resume, include);
            walker.window = 2;
            walker
        };
        let collect = |mut walker: Walker<'_>| {
            let mut positions = Vec::new();
            let mut skipped = Vec::new();
            let mut tail = 0;
            for event in walker.by_ref() {
                assert_eq!(tail, 0, "TailSkipped is the last event");
                match event {
                    WalkEvent::Entry(entry) => {
                        positions.push(entry.components);
                        skipped.push(entry.skipped_before);
                    }
                    WalkEvent::TailSkipped(n) => {
                        assert!(n > 0);
                        tail = n;
                    }
                    WalkEvent::Inaccessible => panic!("synthetic tree is accessible"),
                }
            }
            (positions, skipped, tail, walker.reads)
        };
        let (positions, skipped, tail, reads) = collect(make(None, false, usize::MAX));
        let mut expected = (1..=5)
            .map(|depth| vec![name.clone(); depth])
            .collect::<Vec<_>>();
        let deep = [parents.clone(), vec!["deep.txt".into()]].concat();
        expected.push(deep.clone());
        expected.push([parents.clone(), vec![fitting.clone()]].concat());
        assert_eq!(positions, expected);
        assert_eq!(skipped, [0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(tail, 1);
        assert_eq!(reads, 7); // Base + five directories + the fifth's second window.
        for (index, position) in positions.iter().enumerate() {
            let (remaining, counts, tail, _) =
                collect(make(Some(position.clone()), false, usize::MAX));
            assert_eq!(remaining, positions[index + 1..]);
            assert_eq!(
                counts.iter().sum::<usize>() + tail,
                if index < 5 { 2 } else { 1 }
            );
            let (included, counts, tail, _) =
                collect(make(Some(position.clone()), true, usize::MAX));
            assert_eq!(included, positions[index..]);
            assert_eq!(counts[0], 0);
            assert_eq!(
                counts.iter().sum::<usize>() + tail,
                if index < 5 { 2 } else { 1 }
            );
        }
        let (shallow, counts, tail, reads) = collect(make(None, false, 5));
        assert_eq!(shallow, positions[..5]);
        assert_eq!(counts.iter().sum::<usize>() + tail, 0);
        assert_eq!(reads, 5);
        inner.remove_file("hidden.txt").unwrap();
        drop(inner);
        dir.remove_dir(&name).unwrap();
        for file in ["deep.txt", fitting.as_str(), omitted.as_str()] {
            dir.remove_file(file).unwrap();
        }
        drop(dir);
        for depth in (1..=5).rev() {
            let mut parent = root.dir.try_clone().unwrap();
            for _ in 1..depth {
                parent = parent.open_dir(&name).unwrap();
            }
            parent.remove_dir(&name).unwrap();
        }
    }
}
