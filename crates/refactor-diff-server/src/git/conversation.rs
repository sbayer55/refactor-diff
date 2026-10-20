//! A pull request's conversation as GitHub's GraphQL API describes it: top-level comments,
//! submitted reviews and inline review threads, normalized for the UI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SourceError;

/// The query `GhCli::pr_conversation` runs, one page at a time. `$threads` / `$comments` are the
/// cursors of the two paginated connections; reviews are capped at the latest 100.
pub const CONVERSATION_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!, $threads: String, $comments: String) {
  viewer { login }
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      number url headRefOid reviewDecision viewerDidAuthor
      author { login }
      comments(first: 100, after: $comments) {
        pageInfo { hasNextPage endCursor }
        nodes { ...C }
      }
      reviews(last: 100) {
        nodes { id databaseId author { login avatarUrl } state body bodyHTML submittedAt url viewerDidAuthor }
      }
      reviewThreads(first: 100, after: $threads) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated viewerCanResolve viewerCanUnresolve viewerCanReply
          path line originalLine startLine diffSide
          resolvedBy { login }
          comments(first: 100) { nodes { ...C pullRequestReview { databaseId state } } }
        }
      }
    }
  }
}
fragment C on Comment {
  id author { login avatarUrl } body bodyHTML createdAt
  ... on IssueComment { databaseId url viewerCanUpdate viewerCanDelete }
  ... on PullRequestReviewComment { databaseId url viewerCanUpdate viewerCanDelete }
}
"#;

/// Which REST collection a comment lives in: the PR's conversation or a review thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    Issue,
    Review,
}

impl CommentKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "issue" => Some(Self::Issue),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    /// The REST path of one comment of this kind, with gh's `{owner}/{repo}` placeholders.
    pub fn endpoint(self, id: i64) -> String {
        match self {
            Self::Issue => format!("repos/{{owner}}/{{repo}}/issues/comments/{id}"),
            Self::Review => format!("repos/{{owner}}/{{repo}}/pulls/comments/{id}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    /// GraphQL node id.
    pub id: String,
    /// REST id, used to reply, edit and delete.
    pub database_id: i64,
    /// `None` for a deleted account ("ghost").
    pub author: Option<String>,
    pub avatar_url: Option<String>,
    pub body: String,
    pub body_html: String,
    pub created_at: String,
    pub url: String,
    pub can_edit: bool,
    pub can_delete: bool,
    /// Part of the viewer's pending review on GitHub (only the viewer sees these).
    #[serde(default)]
    pub pending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub path: String,
    /// The line in the PR's current diff; `None` once the thread is outdated.
    pub line: Option<i64>,
    pub original_line: Option<i64>,
    pub start_line: Option<i64>,
    /// `LEFT` (base) or `RIGHT` (head).
    pub side: String,
    pub outdated: bool,
    pub resolved: bool,
    pub resolved_by: Option<String>,
    pub can_resolve: bool,
    pub can_unresolve: bool,
    pub can_reply: bool,
    pub comments: Vec<Comment>,
    /// The report hunk the thread's line falls in, filled in by the route.
    #[serde(default)]
    pub hunk_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewEntry {
    pub id: String,
    pub database_id: i64,
    pub author: Option<String>,
    pub avatar_url: Option<String>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED` or `PENDING`.
    pub state: String,
    pub body: String,
    pub body_html: String,
    pub submitted_at: Option<String>,
    pub url: String,
    pub viewer_did_author: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub number: i64,
    pub url: String,
    pub viewer: String,
    pub viewer_is_author: bool,
    pub author: Option<String>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED` or `None`.
    pub review_decision: Option<String>,
    pub head_sha: String,
    /// Top-level comments, oldest first.
    pub comments: Vec<Comment>,
    /// Submitted reviews, oldest first (the viewer's pending one is in `pending_review`).
    pub reviews: Vec<ReviewEntry>,
    pub threads: Vec<Thread>,
    /// The viewer's unsubmitted review on GitHub, if they started one there.
    pub pending_review: Option<ReviewEntry>,
}

/// Where each paginated connection's last page ended, and whether more pages follow. A
/// finished connection keeps its cursor so later pages of the other one return nothing new.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursors {
    pub threads: Option<String>,
    pub comments: Option<String>,
    pub more_threads: bool,
    pub more_comments: bool,
}

impl Cursors {
    pub fn done(&self) -> bool {
        !self.more_threads && !self.more_comments
    }

    /// This page's cursors, falling back to the previous ones where a page came back empty.
    pub fn after(self, prev: &Cursors) -> Cursors {
        Cursors {
            threads: self.threads.or_else(|| prev.threads.clone()),
            comments: self.comments.or_else(|| prev.comments.clone()),
            ..self
        }
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt_s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn login(v: &Value) -> Option<String> {
    v.get("author").and_then(|a| opt_s(a, "login"))
}

fn nodes<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k)
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// A connection's end cursor and whether it has a next page.
fn page_info(v: &Value, k: &str) -> (Option<String>, bool) {
    match v.get(k).and_then(|c| c.get("pageInfo")) {
        Some(info) => (opt_s(info, "endCursor"), b(info, "hasNextPage")),
        None => (None, false),
    }
}

fn comment(v: &Value) -> Comment {
    Comment {
        id: s(v, "id"),
        database_id: v.get("databaseId").and_then(Value::as_i64).unwrap_or(0),
        author: login(v),
        avatar_url: v.get("author").and_then(|a| opt_s(a, "avatarUrl")),
        body: s(v, "body"),
        body_html: s(v, "bodyHTML"),
        created_at: s(v, "createdAt"),
        url: s(v, "url"),
        can_edit: b(v, "viewerCanUpdate"),
        can_delete: b(v, "viewerCanDelete"),
        pending: v
            .get("pullRequestReview")
            .and_then(|r| r.get("state"))
            .and_then(Value::as_str)
            == Some("PENDING"),
    }
}

fn int(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}

fn thread(v: &Value) -> Thread {
    Thread {
        id: s(v, "id"),
        path: s(v, "path"),
        line: int(v, "line"),
        original_line: int(v, "originalLine"),
        start_line: int(v, "startLine"),
        side: opt_s(v, "diffSide").unwrap_or_else(|| "RIGHT".into()),
        outdated: b(v, "isOutdated"),
        resolved: b(v, "isResolved"),
        resolved_by: v.get("resolvedBy").and_then(|a| opt_s(a, "login")),
        can_resolve: b(v, "viewerCanResolve"),
        can_unresolve: b(v, "viewerCanUnresolve"),
        can_reply: b(v, "viewerCanReply"),
        comments: nodes(v, "comments").map(comment).collect(),
        hunk_id: None,
    }
}

fn review(v: &Value) -> ReviewEntry {
    ReviewEntry {
        id: s(v, "id"),
        database_id: v.get("databaseId").and_then(Value::as_i64).unwrap_or(0),
        author: login(v),
        avatar_url: v.get("author").and_then(|a| opt_s(a, "avatarUrl")),
        state: s(v, "state"),
        body: s(v, "body"),
        body_html: s(v, "bodyHTML"),
        submitted_at: opt_s(v, "submittedAt"),
        url: s(v, "url"),
        viewer_did_author: b(v, "viewerDidAuthor"),
    }
}

impl Conversation {
    /// One page of [`CONVERSATION_QUERY`]'s response, and where the next page starts.
    pub fn from_page(raw: &Value) -> Result<(Self, Cursors), SourceError> {
        if let Some(errors) = raw.get("errors").and_then(Value::as_array) {
            let msgs: Vec<String> = errors.iter().map(|e| s(e, "message")).collect();
            return Err(SourceError::Message(format!(
                "GitHub refused the query: {}",
                msgs.join("; ")
            )));
        }
        let data = raw.get("data").unwrap_or(raw);
        let pr = data
            .get("repository")
            .and_then(|r| r.get("pullRequest"))
            .filter(|p| !p.is_null())
            .ok_or_else(|| SourceError::Message("GitHub returned no pull request.".into()))?;
        let mut reviews: Vec<ReviewEntry> = nodes(pr, "reviews").map(review).collect();
        let pending_at = reviews
            .iter()
            .position(|r| r.state == "PENDING" && r.viewer_did_author);
        let pending_review = pending_at.map(|i| reviews.remove(i));
        // Other people's pending reviews are never visible; drop anything else unsubmitted.
        reviews.retain(|r| r.state != "PENDING");
        let conv = Self {
            number: pr.get("number").and_then(Value::as_i64).unwrap_or(0),
            url: s(pr, "url"),
            viewer: data
                .get("viewer")
                .map(|v| s(v, "login"))
                .unwrap_or_default(),
            viewer_is_author: b(pr, "viewerDidAuthor"),
            author: login(pr),
            review_decision: opt_s(pr, "reviewDecision"),
            head_sha: s(pr, "headRefOid"),
            comments: nodes(pr, "comments").map(comment).collect(),
            reviews,
            threads: nodes(pr, "reviewThreads").map(thread).collect(),
            pending_review,
        };
        let (threads, more_threads) = page_info(pr, "reviewThreads");
        let (comments, more_comments) = page_info(pr, "comments");
        let cursors = Cursors {
            threads,
            comments,
            more_threads,
            more_comments,
        };
        Ok((conv, cursors))
    }

    /// Append a later page's comments and threads (the rest of a page repeats the first).
    pub fn extend(&mut self, page: Conversation) {
        self.comments.extend(page.comments);
        self.threads.extend(page.threads);
    }
}

/// One inline comment of a submitted review (`POST pulls/{n}/reviews`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewPayloadComment {
    pub path: String,
    pub line: i64,
    pub side: String,
    pub body: String,
}

/// `POST repos/{owner}/{repo}/pulls/{n}/reviews`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewPayload {
    pub commit_id: String,
    pub body: String,
    /// `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    pub event: String,
    pub comments: Vec<ReviewPayloadComment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(threads_next: bool, comments_next: bool) -> Value {
        let issue_comment = json!({
            "id": "IC_1", "databaseId": 11, "author": null, "body": "hi", "bodyHTML": "<p>hi</p>",
            "createdAt": "2026-10-01T00:00:00Z", "url": "u",
            "viewerCanUpdate": false, "viewerCanDelete": false
        });
        let reviews = json!([
            {"id": "R1", "databaseId": 1, "author": {"login": "them"}, "state": "APPROVED",
             "body": "", "bodyHTML": "", "submittedAt": "2026-10-02T00:00:00Z", "url": "r1",
             "viewerDidAuthor": false},
            {"id": "R2", "databaseId": 2, "author": {"login": "me"}, "state": "PENDING",
             "body": "", "bodyHTML": "", "submittedAt": null, "url": "r2", "viewerDidAuthor": true}
        ]);
        let thread_comment = json!({
            "id": "PRRC_1", "databaseId": 21, "author": {"login": "me", "avatarUrl": "av"},
            "body": "x", "bodyHTML": "x", "createdAt": "t", "url": "c",
            "viewerCanUpdate": true, "viewerCanDelete": true,
            "pullRequestReview": {"databaseId": 2, "state": "PENDING"}
        });
        let thread = json!({
            "id": "PRRT_1", "isResolved": false, "isOutdated": true,
            "viewerCanResolve": true, "viewerCanUnresolve": false, "viewerCanReply": true,
            "path": "a.py", "line": null, "originalLine": 4, "startLine": null,
            "diffSide": "LEFT", "resolvedBy": null, "comments": {"nodes": [thread_comment]}
        });
        let pr = json!({
            "number": 7, "url": "https://example.test/pull/7", "headRefOid": "abc",
            "reviewDecision": null, "viewerDidAuthor": false, "author": {"login": "them"},
            "comments": {
                "pageInfo": {"hasNextPage": comments_next, "endCursor": "C1"},
                "nodes": [issue_comment]
            },
            "reviews": {"nodes": reviews},
            "reviewThreads": {
                "pageInfo": {"hasNextPage": threads_next, "endCursor": "T1"},
                "nodes": [thread]
            }
        });
        json!({"data": {"viewer": {"login": "me"}, "repository": {"pullRequest": pr}}})
    }

    #[test]
    fn parses_a_page() {
        let (c, cursors) = Conversation::from_page(&page(false, false)).unwrap();
        assert!(cursors.done());
        assert_eq!(c.viewer, "me");
        assert_eq!(c.author.as_deref(), Some("them"));
        assert_eq!(c.review_decision, None);
        assert_eq!(c.comments[0].author, None, "a deleted account is a ghost");
        assert_eq!(c.comments[0].database_id, 11);
        assert_eq!(c.reviews.len(), 1);
        assert_eq!(c.pending_review.as_ref().map(|r| r.database_id), Some(2));
        let t = &c.threads[0];
        assert_eq!(
            (t.line, t.original_line, t.side.as_str()),
            (None, Some(4), "LEFT")
        );
        assert!(t.outdated && !t.resolved && t.can_resolve);
        assert!(t.comments[0].pending && t.comments[0].can_edit);
    }

    #[test]
    fn reports_cursors_and_merges_pages() {
        let (mut c, cursors) = Conversation::from_page(&page(true, false)).unwrap();
        assert!(cursors.more_threads && !cursors.more_comments);
        assert_eq!(cursors.threads.as_deref(), Some("T1"));
        assert_eq!(cursors.comments.as_deref(), Some("C1"));
        let empty = Cursors::default().after(&cursors);
        assert_eq!(
            empty.threads.as_deref(),
            Some("T1"),
            "an empty page keeps the old cursor"
        );
        assert!(empty.done());
        let (next, _) = Conversation::from_page(&page(false, false)).unwrap();
        c.extend(next);
        assert_eq!(c.threads.len(), 2);
        assert_eq!(c.comments.len(), 2);
        assert_eq!(c.reviews.len(), 1, "reviews come from the first page only");
    }

    #[test]
    fn graphql_errors_and_missing_prs_are_errors() {
        let err = Conversation::from_page(&json!({"errors": [{"message": "nope"}]})).unwrap_err();
        assert!(err.to_string().contains("nope"));
        let none = json!({"data": {"repository": {"pullRequest": null}}});
        assert!(Conversation::from_page(&none).is_err());
    }
}
