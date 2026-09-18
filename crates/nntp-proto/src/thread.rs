//! Grouping articles into conversations.
//!
//! An implementation of [Jamie Zawinski's threading algorithm][jwz], which exists because
//! `References` chains in the wild are broken. Parents expire, are cancelled, or were
//! never carried by this server; senders trim the chain, reverse it, or omit it entirely;
//! mail-to-news gateways rewrite it. An indent computed by trusting the headers would be a
//! confident lie, which is why the reader shipped a flat list with a `\u{203a}` marker until
//! this existed.
//!
//! The algorithm is tolerant by construction:
//!
//! - a reply whose parent is missing still groups with its siblings, through a container
//!   that stands in for the article nobody has;
//! - a reply that names no parent at all can still be pulled into its thread by subject;
//! - a chain that refers to itself — malformed, but it happens — is broken rather than
//!   followed.
//!
//! This module is pure: records in, a tree out, no IO and no opinion about how the tree is
//! drawn. It lives beside the rest of the header interpretation rather than in the reader
//! because nothing in it is about terminals, and a second front end would want the same
//! grouping.
//!
//! [jwz]: https://www.jwz.org/doc/threading.html

use std::collections::HashMap;

use crate::message_id::MessageId;
use crate::overview::OverviewRecord;

/// How many of an article's references are considered, counting back from its parent.
///
/// A `References` header is unbounded in principle and occasionally absurd in practice.
/// The nearest ancestors are the ones that matter for grouping — the distant ones are
/// almost always already in the same thread through a nearer link — so the chain is used
/// from the back. The cap keeps one hostile or broken article from filling the arena with
/// placeholders for messages nobody has.
pub const MAX_REFERENCES: usize = 64;

/// The deepest level a node can be at: a root is level 0, its replies level 1.
///
/// Anything below the limit is attached to the deepest ancestor the limit allows, rather
/// than dropped: a display cannot usefully indent a hundred levels anyway, and losing the
/// article would be the worse failure.
pub const MAX_DEPTH: usize = 32;

/// One node of a threaded list.
///
/// `article` is an index into the slice passed to [`thread`], or `None` for a container
/// standing in for an article that is not present — an expired root, or a parent this
/// server never carried. An empty node is kept only when it holds more than one child,
/// because that is the only case where it says something a reader needs: *these replies
/// belong together, and what they reply to is gone.*
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadNode {
    /// Index into the records that were threaded, if this node has an article.
    pub article: Option<usize>,
    /// Replies, oldest first.
    pub children: Vec<ThreadNode>,
}

impl ThreadNode {
    /// The number of articles in this node and everything below it.
    ///
    /// Empty containers do not count: a thread of three replies to a missing root has
    /// three articles, not four.
    pub fn len(&self) -> usize {
        usize::from(self.article.is_some()) + self.children.iter().map(Self::len).sum::<usize>()
    }

    /// Whether the node holds no article at all, here or below.
    ///
    /// Always `false` for a node [`thread`] returns; pruning removes those. It exists for
    /// callers that build or filter trees themselves.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visits every node depth-first, oldest first, with its depth.
    ///
    /// The order a reader shows: a parent immediately above its replies.
    pub fn walk(&self, depth: usize, visit: &mut impl FnMut(&Self, usize)) {
        visit(self, depth);
        for child in &self.children {
            child.walk(depth + 1, visit);
        }
    }
}

/// Groups records into threads, roots first.
///
/// Roots are returned in the order of their oldest article, and replies in the order they
/// were given — which for overview records is article-number order, the closest thing a
/// server offers to the order things were said.
///
/// Records with no message-id are still returned: each becomes a root of its own, because
/// an article that cannot be referred to cannot be a parent, and dropping it would hide it
/// from the reader entirely.
pub fn thread(records: &[OverviewRecord]) -> Vec<ThreadNode> {
    let mut arena = Arena::default();

    // 1. A container per article, and one per message-id that is only ever referred to.
    for (index, record) in records.iter().enumerate() {
        arena.insert(index, record);
    }

    // 2. Link each article to the parent its references name.
    for (index, record) in records.iter().enumerate() {
        arena.link_references(index, record);
    }

    // 3. Roots.
    let mut roots = arena.roots();

    // 5. Subject grouping, the last resort for threads whose links are missing entirely.
    // Out of order on purpose: grouping changes who is a root, so it has to happen before
    // the depth cap and before the tree is built.
    group_by_subject(&mut arena, &mut roots, records);

    arena.clamp_depth(&roots);

    // 4. Build, pruning the empty containers on the way out.
    roots.into_iter().filter_map(|id| arena.build(id)).collect()
}

/// A container as the algorithm describes it: an article, its parent and its replies.
///
/// Held in an arena and referred to by index rather than by `Rc<RefCell<_>>`. The graph
/// being built is arbitrary — cycles included, since that is one of the malformations
/// being defended against — and an arena makes an ancestor walk a loop over integers
/// instead of a borrow that can panic at runtime.
#[derive(Debug, Default)]
struct Container {
    article: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
}

#[derive(Debug, Default)]
struct Arena {
    containers: Vec<Container>,
    /// Where each message-id lives. The first article claiming an id keeps it.
    by_id: HashMap<String, usize>,
    /// Container per input record, in record order.
    by_article: Vec<usize>,
}

impl Arena {
    fn new_container(&mut self, article: Option<usize>) -> usize {
        self.containers.push(Container {
            article,
            parent: None,
            children: Vec::new(),
        });
        self.containers.len() - 1
    }

    /// Adds a container for one record.
    fn insert(&mut self, index: usize, record: &OverviewRecord) {
        let id = match &record.message_id {
            Some(id) => id.as_str().to_owned(),
            // No message-id: nothing can refer to it, so it needs no entry in the table.
            None => {
                let container = self.new_container(Some(index));
                self.by_article.push(container);
                return;
            }
        };

        match self.by_id.get(&id).copied() {
            // A container made earlier by a reference to this article: it was waiting for
            // exactly this.
            Some(existing) if self.article_of(existing).is_none() => {
                if let Some(container) = self.containers.get_mut(existing) {
                    container.article = Some(index);
                }
                self.by_article.push(existing);
            }

            // Two articles with the same message-id. It happens — a crosspost the server
            // numbers twice, or a duplicate injection. The second gets a container of its
            // own and no claim on the id, so it is shown rather than silently merged into
            // the first.
            Some(_) => {
                let container = self.new_container(Some(index));
                self.by_article.push(container);
            }

            None => {
                let container = self.new_container(Some(index));
                self.by_id.insert(id, container);
                self.by_article.push(container);
            }
        }
    }

    /// Container for a message-id, creating an empty one if it is only referred to.
    fn container_for_id(&mut self, id: &MessageId) -> usize {
        if let Some(existing) = self.by_id.get(id.as_str()).copied() {
            return existing;
        }
        let container = self.new_container(None);
        self.by_id.insert(id.as_str().to_owned(), container);
        container
    }

    /// Links an article to its references, and its references to each other.
    fn link_references(&mut self, index: usize, record: &OverviewRecord) {
        let Some(&child) = self.by_article.get(index) else {
            return;
        };

        // Oldest first in the header; the nearest ancestors are at the end, so a chain
        // longer than the cap is used from the back.
        let references = record
            .references
            .iter()
            .skip(record.references.len().saturating_sub(MAX_REFERENCES));

        let mut previous: Option<usize> = None;
        for reference in references {
            let current = self.container_for_id(reference);
            if let Some(parent) = previous {
                self.set_parent(current, parent);
            }
            previous = Some(current);
        }

        if let Some(parent) = previous {
            self.set_parent(child, parent);
        }
    }

    /// Makes `parent` the parent of `child`, unless that would be a lie or a loop.
    ///
    /// An existing parent is left alone: the first link wins, which is the one built from
    /// the article's own header rather than from somebody else's reference to it.
    fn set_parent(&mut self, child: usize, parent: usize) {
        if child == parent || self.parent_of(child).is_some() {
            return;
        }
        if self.is_ancestor(child, parent) {
            // `parent` already descends from `child`: the references describe a cycle.
            // Leaving both where they are keeps every article reachable, which losing one
            // of them would not.
            return;
        }

        if let Some(container) = self.containers.get_mut(child) {
            container.parent = Some(parent);
        }
        if let Some(container) = self.containers.get_mut(parent) {
            container.children.push(child);
        }
    }

    fn article_of(&self, id: usize) -> Option<usize> {
        self.containers.get(id).and_then(|c| c.article)
    }

    fn parent_of(&self, id: usize) -> Option<usize> {
        self.containers.get(id).and_then(|c| c.parent)
    }

    /// Whether `candidate` is `id` or one of its ancestors.
    ///
    /// The step count is a backstop: the parent links are supposed to be acyclic by the
    /// time this is asked, and this is what is asked to keep them that way.
    fn is_ancestor(&self, candidate: usize, id: usize) -> bool {
        let mut current = Some(id);
        for _ in 0..=self.containers.len() {
            match current {
                Some(node) if node == candidate => return true,
                Some(node) => current = self.parent_of(node),
                None => return false,
            }
        }
        true
    }

    /// The containers with no parent, in the order of their oldest article.
    fn roots(&self) -> Vec<usize> {
        let mut roots: Vec<usize> = (0..self.containers.len())
            .filter(|id| self.parent_of(*id).is_none())
            .filter(|id| self.first_article(*id).is_some())
            .collect();

        roots.sort_by_key(|id| self.first_article(*id));
        roots
    }

    /// The lowest record index anywhere in this container's subtree.
    ///
    /// `None` for a container that holds nothing at all: a placeholder created by a
    /// reference to an article nobody has, whose whole subtree is placeholders. Those are
    /// what step 4 of the algorithm prunes.
    fn first_article(&self, id: usize) -> Option<usize> {
        let mut best = self.article_of(id);
        let mut stack = vec![id];
        let mut steps = 0usize;

        while let Some(current) = stack.pop() {
            steps += 1;
            if steps > self.containers.len() + 1 {
                break;
            }
            if let Some(container) = self.containers.get(current) {
                if let Some(article) = container.article {
                    best = Some(best.map_or(article, |found: usize| found.min(article)));
                }
                stack.extend(container.children.iter().copied());
            }
        }

        best
    }

    /// Flattens anything nested deeper than [`MAX_DEPTH`] onto the deepest ancestor the
    /// limit allows.
    ///
    /// Two reasons, and the second is the important one. A display cannot usefully indent
    /// a hundred levels; and [`Self::build`] is recursive, so without a bound on the depth
    /// a group carrying one very long reply chain — which is a thing a server can be made
    /// to serve — would overflow the stack rather than draw a deep thread. The articles
    /// are kept in every case: losing one is worse than showing it at the wrong indent.
    ///
    /// Iterative, for the same reason it exists.
    fn clamp_depth(&mut self, roots: &[usize]) {
        let mut seen = vec![false; self.containers.len()];
        let mut queue: std::collections::VecDeque<(usize, usize)> =
            roots.iter().map(|id| (*id, 0usize)).collect();

        while let Some((id, depth)) = queue.pop_front() {
            match seen.get(id) {
                Some(true) | None => continue,
                Some(false) => {}
            }
            if let Some(slot) = seen.get_mut(id) {
                *slot = true;
            }

            let children = self
                .containers
                .get(id)
                .map(|container| container.children.clone())
                .unwrap_or_default();

            if depth + 1 < MAX_DEPTH {
                queue.extend(children.into_iter().map(|child| (child, depth + 1)));
                continue;
            }

            // At the limit: everything below becomes a direct child of this node.
            let mut descendants = Vec::new();
            let mut stack = children;
            while let Some(node) = stack.pop() {
                match seen.get(node) {
                    Some(true) | None => continue,
                    Some(false) => {}
                }
                if let Some(slot) = seen.get_mut(node) {
                    *slot = true;
                }

                if let Some(container) = self.containers.get_mut(node) {
                    let grandchildren = std::mem::take(&mut container.children);
                    container.parent = Some(id);
                    stack.extend(grandchildren);
                    descendants.push(node);
                }
            }

            descendants.sort_by_key(|node| self.first_article(*node));
            if let Some(container) = self.containers.get_mut(id) {
                container.children = descendants;
            }
        }
    }

    /// Turns a container into the tree a caller sees, pruning empty containers.
    ///
    /// An empty container with one child is replaced by that child — it says nothing. An
    /// empty container with several children is kept, because it is the only way to say
    /// that those replies belong together and their parent is missing.
    ///
    /// Recursive, which is safe only because [`Self::clamp_depth`] has already bounded how
    /// deep the tree can be.
    fn build(&self, id: usize) -> Option<ThreadNode> {
        let container = self.containers.get(id)?;

        let mut children: Vec<ThreadNode> = container
            .children
            .iter()
            .filter_map(|child| self.build(*child))
            .collect();

        children.sort_by_key(ThreadNode::first_article);

        if container.article.is_none() {
            return match children.len() {
                0 => None,
                1 => children.pop(),
                _ => Some(ThreadNode {
                    article: None,
                    children,
                }),
            };
        }

        Some(ThreadNode {
            article: container.article,
            children,
        })
    }
}

impl ThreadNode {
    /// The lowest record index in this node's subtree, for ordering.
    fn first_article(&self) -> usize {
        let mut best = self.article.unwrap_or(usize::MAX);
        for child in &self.children {
            best = best.min(child.first_article());
        }
        best
    }
}

/// Pulls together roots that share a subject.
///
/// The last resort, and the part that rescues threads from mail-to-news gateways and from
/// clients that send no `References` at all. It is deliberately narrow: only roots are
/// considered, only subjects that survive stripping to something non-empty, and a root
/// whose subject is *not* a reply is preferred as the parent. Merging on subject alone is
/// how unrelated articles end up in one another's conversations, so it does as little as
/// it can get away with.
fn group_by_subject(arena: &mut Arena, roots: &mut Vec<usize>, records: &[OverviewRecord]) {
    // Subject -> the root that will hold the others.
    let mut holders: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<usize> = Vec::new();

    for id in roots.iter().copied() {
        let Some(article) = arena.first_article(id) else {
            continue;
        };
        let Some(record) = records.get(article) else {
            continue;
        };

        let (subject, is_reply) = normalise_subject(&record.subject);
        if subject.is_empty() {
            continue;
        }

        match holders.get(&subject).copied() {
            None => {
                holders.insert(subject, id);
            }
            Some(holder) if holder == id => {}
            Some(holder) => {
                // A root that announces itself as a reply belongs under one that does
                // not. Where both or neither are replies, the older article holds, which
                // keeps the thread's first article at its root.
                let holder_is_reply = arena
                    .first_article(holder)
                    .and_then(|index| records.get(index))
                    .is_some_and(|record| normalise_subject(&record.subject).1);

                let (parent, child) = if is_reply && !holder_is_reply {
                    (holder, id)
                } else if holder_is_reply && !is_reply {
                    holders.insert(subject, id);
                    (id, holder)
                } else if arena.first_article(holder) <= arena.first_article(id) {
                    (holder, id)
                } else {
                    holders.insert(subject, id);
                    (id, holder)
                };

                arena.set_parent(child, parent);
                merged.push(child);
            }
        }
    }

    roots.retain(|id| !merged.contains(id));
}

/// Strips reply prefixes from a subject, reporting whether it had any.
///
/// `Re:` in the languages a Usenet reader actually meets, repeated any number of times and
/// with the mailing-list `Re[2]:` form, plus a leading `[list-name]` tag — which gateways
/// add and which would otherwise split every thread they touch in two. Forwarding prefixes
/// are deliberately not stripped: a forward is a new article, not a reply.
pub fn normalise_subject(subject: &str) -> (String, bool) {
    const PREFIXES: [&str; 8] = ["re", "aw", "sv", "vs", "res", "odp", "ref", "antw"];

    let mut rest = subject.trim();
    let mut was_reply = false;

    loop {
        // A list tag, which is not a reply marker but does sit in front of one.
        if let Some(stripped) = rest.strip_prefix('[')
            && let Some((tag, tail)) = stripped.split_once(']')
            && !tag.is_empty()
            && !tag.contains('[')
        {
            rest = tail.trim_start();
            continue;
        }

        let Some((head, tail)) = rest.split_once(':') else {
            break;
        };

        // `Re[2]` and `Re(2)` are the same marker with a counter attached.
        let head = head.trim();
        let base = head
            .split_once(['[', '('])
            .map_or(head, |(before, _)| before)
            .trim();

        if PREFIXES
            .iter()
            .any(|prefix| base.eq_ignore_ascii_case(prefix))
        {
            rest = tail.trim_start();
            was_reply = true;
            continue;
        }

        break;
    }

    (rest.trim().to_owned(), was_reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::OverviewFmt;

    /// One overview record: number, subject, and the references it names.
    fn record(number: u64, subject: &str, references: &[u64]) -> OverviewRecord {
        let refs = references
            .iter()
            .map(|n| format!("<{n}@example.org>"))
            .collect::<Vec<_>>()
            .join(" ");
        let line = format!(
            "{number}\t{subject}\ta@x\tThu, 1 Jan 2026 00:00:00 +0000\t<{number}@example.org>\t{refs}\t10\t1"
        );
        OverviewRecord::parse(line.as_bytes(), &OverviewFmt::standard()).expect("a record")
    }

    /// The tree as a flat list of `(depth, article number or None)`, in display order.
    fn shape(roots: &[ThreadNode], records: &[OverviewRecord]) -> Vec<(usize, Option<u64>)> {
        let mut out = Vec::new();
        for root in roots {
            root.walk(0, &mut |node, depth| {
                let number = node
                    .article
                    .and_then(|index| records.get(index))
                    .map(|record| record.number);
                out.push((depth, number));
            });
        }
        out
    }

    #[test]
    fn a_flat_group_is_a_list_of_roots() {
        let records = vec![
            record(1, "one", &[]),
            record(2, "two", &[]),
            record(3, "three", &[]),
        ];
        let roots = thread(&records);

        assert_eq!(roots.len(), 3);
        assert_eq!(
            shape(&roots, &records),
            vec![(0, Some(1)), (0, Some(2)), (0, Some(3))]
        );
    }

    #[test]
    fn replies_nest_under_the_article_they_answer() {
        let records = vec![
            record(1, "question", &[]),
            record(2, "Re: question", &[1]),
            record(3, "Re: question", &[1, 2]),
        ];

        assert_eq!(
            shape(&thread(&records), &records),
            vec![(0, Some(1)), (1, Some(2)), (2, Some(3))]
        );
    }

    #[test]
    fn a_thread_whose_root_has_expired_still_groups() {
        // The reason the algorithm has containers at all. Nobody has article 1; its two
        // replies must still be shown together rather than as two unrelated articles.
        let records = vec![
            record(2, "Re: gone", &[1]),
            record(3, "Re: gone", &[1]),
            record(4, "unrelated", &[]),
        ];

        assert_eq!(
            shape(&thread(&records), &records),
            vec![
                // The empty container is the missing root, kept because it holds more
                // than one child.
                (0, None),
                (1, Some(2)),
                (1, Some(3)),
                (0, Some(4)),
            ]
        );
    }

    #[test]
    fn a_lone_reply_to_a_missing_parent_is_not_wrapped_in_an_empty_node() {
        // One child under a placeholder says nothing the indent does not already say, so
        // the placeholder goes.
        let records = vec![record(2, "Re: gone", &[1])];

        assert_eq!(shape(&thread(&records), &records), vec![(0, Some(2))]);
    }

    #[test]
    fn a_subject_only_thread_groups() {
        // No References anywhere — a client that sends none, or a gateway that strips
        // them. Subject is all there is to go on.
        let records = vec![
            record(1, "the plan", &[]),
            record(2, "Re: the plan", &[]),
            record(3, "Re: Re: the plan", &[]),
        ];

        assert_eq!(
            shape(&thread(&records), &records),
            vec![(0, Some(1)), (1, Some(2)), (1, Some(3))]
        );
    }

    #[test]
    fn subject_grouping_does_not_reparent_articles_that_have_a_thread() {
        // Two conversations that happen to share a subject, each with its own root and
        // its own references. Subject grouping must not staple them together.
        let records = vec![
            record(1, "status", &[]),
            record(2, "Re: status", &[1]),
            record(3, "Re: status", &[1]),
        ];

        let roots = thread(&records);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots.first().map(ThreadNode::len), Some(3));
    }

    #[test]
    fn a_reference_cycle_does_not_loop() {
        // Malformed, and it happens: each article claims the other as its parent. The
        // requirement is that threading terminates and loses nobody.
        let mut first = record(1, "a", &[]);
        first.references = vec![MessageId::parse("<2@example.org>").expect("id")];
        let mut second = record(2, "b", &[]);
        second.references = vec![MessageId::parse("<1@example.org>").expect("id")];

        let records = vec![first, second];
        let roots = thread(&records);

        let total: usize = roots.iter().map(ThreadNode::len).sum();
        assert_eq!(total, 2, "both articles are still reachable");
    }

    #[test]
    fn an_article_that_refers_to_itself_is_its_own_root() {
        let mut only = record(1, "a", &[]);
        only.references = vec![MessageId::parse("<1@example.org>").expect("id")];

        assert_eq!(shape(&thread(&[only.clone()]), &[only]), vec![(0, Some(1))]);
    }

    #[test]
    fn an_article_with_no_message_id_is_kept_as_its_own_root() {
        let mut orphan = record(1, "anonymous", &[]);
        orphan.message_id = None;
        let records = vec![orphan, record(2, "other", &[])];

        assert_eq!(
            shape(&thread(&records), &records),
            vec![(0, Some(1)), (0, Some(2))]
        );
    }

    #[test]
    fn two_articles_with_one_message_id_are_both_shown() {
        // A duplicate injection. Merging them would hide one from the reader.
        let records = vec![record(1, "first", &[]), record(1, "second", &[])];
        let roots = thread(&records);

        assert_eq!(roots.iter().map(ThreadNode::len).sum::<usize>(), 2);
    }

    #[test]
    fn roots_come_out_in_the_order_of_their_oldest_article() {
        let records = vec![
            record(1, "old thread", &[]),
            record(2, "new thread", &[]),
            record(3, "Re: old thread", &[1]),
        ];

        assert_eq!(
            shape(&thread(&records), &records),
            vec![(0, Some(1)), (1, Some(3)), (0, Some(2))]
        );
    }

    #[test]
    fn a_long_reference_chain_is_used_from_the_nearest_end() {
        // Only the last MAX_REFERENCES entries are considered, so the parent link — the
        // one that matters — survives a chain of any length.
        let ancestors: Vec<u64> = (1..=(MAX_REFERENCES as u64 + 20)).collect();
        let mut records: Vec<OverviewRecord> =
            ancestors.iter().map(|n| record(*n, "deep", &[])).collect();
        let last = *ancestors.last().expect("an ancestor");
        records.push(record(last + 1, "Re: deep", &ancestors));

        let roots = thread(&records);
        let total: usize = roots.iter().map(ThreadNode::len).sum();
        assert_eq!(total, records.len(), "nobody was lost");
    }

    #[test]
    fn nesting_is_capped_rather_than_unbounded() {
        // A thread deeper than the limit keeps every article; it just stops indenting.
        let depth = MAX_DEPTH + 10;
        let mut records = vec![record(1, "root", &[])];
        for number in 2..=(depth as u64 + 1) {
            let chain: Vec<u64> = (1..number).collect();
            records.push(record(number, "Re: root", &chain));
        }

        let roots = thread(&records);
        let mut deepest = 0usize;
        for root in &roots {
            root.walk(0, &mut |_, level| deepest = deepest.max(level));
        }

        assert!(deepest <= MAX_DEPTH, "indented {deepest} levels");
        assert_eq!(
            roots.iter().map(ThreadNode::len).sum::<usize>(),
            records.len(),
            "nobody was lost to the cap"
        );
    }

    #[test]
    fn a_very_long_reply_chain_does_not_overflow_the_stack() {
        // The reason the depth cap is applied iteratively before the tree is built. A
        // group can carry a chain far longer than any stack will recurse over, and
        // "threading made the reader die" is not an acceptable answer to badly shaped
        // traffic.
        const CHAIN: u64 = 5_000;

        let mut records = vec![record(1, "root", &[])];
        for number in 2..=CHAIN {
            records.push(record(number, "Re: root", &[number - 1]));
        }

        let roots = thread(&records);

        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots.iter().map(ThreadNode::len).sum::<usize>(),
            records.len()
        );
    }

    #[test]
    fn counts_articles_and_not_placeholders() {
        let records = vec![record(2, "Re: gone", &[1]), record(3, "Re: gone", &[1])];
        let roots = thread(&records);

        assert_eq!(roots.len(), 1);
        assert_eq!(roots.first().map(ThreadNode::len), Some(2));
    }

    #[test]
    fn strips_the_reply_prefixes_a_reader_actually_meets() {
        for (input, expected) in [
            ("Re: hello", "hello"),
            ("RE: hello", "hello"),
            ("Re: Re: hello", "hello"),
            ("Re[2]: hello", "hello"),
            ("AW: hello", "hello"),
            ("Sv: hello", "hello"),
            ("Odp: hello", "hello"),
            ("[rust-users] Re: hello", "hello"),
            ("Re: [rust-users] hello", "hello"),
        ] {
            let (subject, was_reply) = normalise_subject(input);
            assert_eq!(subject, expected, "{input}");
            assert!(
                was_reply || input.starts_with('['),
                "{input} should read as a reply"
            );
        }
    }

    #[test]
    fn leaves_a_subject_that_only_looks_like_a_prefix_alone() {
        // The colon is not enough: these are subjects, not reply markers, and stripping
        // them would merge unrelated threads.
        for input in [
            "Reminder: the meeting",
            "Rust: a first look",
            "Fwd: the patch",
            "Résumé: hello",
        ] {
            let (subject, was_reply) = normalise_subject(input);
            assert_eq!(subject, input, "{input} was rewritten");
            assert!(!was_reply, "{input} was read as a reply");
        }
    }

    #[test]
    fn an_empty_subject_after_stripping_does_not_group() {
        // "Re:" and nothing else groups every such article into one bogus thread.
        let records = vec![record(1, "Re:", &[]), record(2, "Re:", &[])];
        let roots = thread(&records);

        assert_eq!(roots.len(), 2);
    }

    #[test]
    fn threading_nothing_yields_nothing() {
        assert!(thread(&[]).is_empty());
    }
}
