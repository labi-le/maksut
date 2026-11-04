use crate::*;

fn registry(max_entries: usize, ttl_secs: u64) -> ResearchRegistry {
    ResearchRegistry::new(max_entries, Duration::from_secs(ttl_secs))
}

fn chat_filter(chat_id: i64) -> SnapshotFilter<'static> {
    SnapshotFilter {
        chat_id: ChatId(chat_id),
        all_chats: false,
        owner: false,
        research_id: None,
    }
}

#[tokio::test]
async fn registry_tracks_progress_and_terminal_status() {
    let registry = registry(8, 60);
    let job = registry
        .start(ChatId(10), UserId(20), "вопрос про rust", 3)
        .await
        .unwrap();
    let id = job.id().to_owned();
    assert_eq!(id, "res-1");

    job.progress(1, 2).await;
    let (rows, truncated) = registry.snapshots(chat_filter(10)).await;
    assert!(!truncated);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].user_id, UserId(20));
    assert_eq!(rows[0].status, ResearchStatus::Running);
    assert_eq!(rows[0].completed_queries, 1);
    assert_eq!(rows[0].total_queries, 3);
    assert_eq!(rows[0].sources, 2);
    assert_eq!(rows[0].question, "вопрос про rust");

    job.finish(ResearchStatus::Completed, 3, 5).await;
    let (rows, _) = registry.snapshots(chat_filter(10)).await;
    assert_eq!(rows[0].status, ResearchStatus::Completed);
    assert_eq!(rows[0].completed_queries, 3);
    assert_eq!(rows[0].sources, 5);
}

#[tokio::test]
async fn registry_scopes_rows_to_the_chat_and_filters_by_id() {
    let registry = registry(8, 60);
    let first = registry
        .start(ChatId(10), UserId(20), "первый", 2)
        .await
        .unwrap();
    first.finish(ResearchStatus::Completed, 2, 1).await;
    let second = registry
        .start(ChatId(11), UserId(21), "второй", 2)
        .await
        .unwrap();
    second.finish(ResearchStatus::Completed, 2, 1).await;

    let (own, _) = registry.snapshots(chat_filter(10)).await;
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].question, "первый");

    let (all, _) = registry
        .snapshots(SnapshotFilter {
            all_chats: true,
            ..chat_filter(10)
        })
        .await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].question, "второй", "newest pass first");

    let (one, _) = registry
        .snapshots(SnapshotFilter {
            research_id: Some("res-1"),
            ..chat_filter(10)
        })
        .await;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].id, "res-1");
    assert_eq!(one[0].user_id, UserId(20));

    let (hidden, _) = registry
        .snapshots(SnapshotFilter {
            research_id: Some("res-2"),
            ..chat_filter(10)
        })
        .await;
    assert!(
        hidden.is_empty(),
        "an id from another chat stays invisible to a participant"
    );

    let (owner_view, _) = registry
        .snapshots(SnapshotFilter {
            research_id: Some("res-2"),
            owner: true,
            ..chat_filter(10)
        })
        .await;
    assert_eq!(owner_view.len(), 1);
    assert_eq!(owner_view[0].question, "второй");
}

#[tokio::test]
async fn registry_refuses_stranger_cancel_and_allows_author_and_owner() {
    let registry = registry(8, 60);
    let job = registry
        .start(ChatId(10), UserId(20), "вопрос", 2)
        .await
        .unwrap();
    let id = job.id().to_owned();

    let stranger = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(30),
            owner: false,
            research_id: None,
        })
        .await;
    assert_eq!(
        stranger.unwrap_err(),
        "нет активного ресерча",
        "a stranger without an id must not be handed someone else's pass"
    );
    let stolen = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(30),
            owner: false,
            research_id: Some(&id),
        })
        .await;
    assert_eq!(stolen.unwrap_err(), "чужой ресерч отменить нельзя");

    let by_owner = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(30),
            owner: true,
            research_id: Some(&id),
        })
        .await
        .unwrap();
    assert_eq!(by_owner.status, ResearchStatus::Canceled);
    let mut receiver = job.receiver();
    receiver.changed().await.unwrap();
    assert!(*receiver.borrow());
    job.finish(ResearchStatus::Canceled, 0, 0).await;

    let reloaded = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(20),
            owner: false,
            research_id: Some(&id),
        })
        .await;
    assert_eq!(reloaded.unwrap_err(), "ресерч res-1 уже завершён: canceled");

    let second = registry
        .start(ChatId(10), UserId(20), "второй", 2)
        .await
        .unwrap();
    let by_author = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(20),
            owner: false,
            research_id: None,
        })
        .await
        .unwrap();
    assert_eq!(by_author.id, second.id());
    second.finish(ResearchStatus::Canceled, 0, 0).await;
}

#[tokio::test]
async fn registry_hides_other_chats_from_cancel_by_id() {
    let registry = registry(8, 60);
    let job = registry
        .start(ChatId(10), UserId(20), "вопрос", 2)
        .await
        .unwrap();
    let id = job.id().to_owned();

    let foreign = registry
        .cancel(CancelRequest {
            chat_id: ChatId(99),
            user_id: UserId(20),
            owner: false,
            research_id: Some(&id),
        })
        .await;
    assert_eq!(foreign.unwrap_err(), format!("ресерч {id} не найден"));
    job.finish(ResearchStatus::Completed, 2, 1).await;
}

#[tokio::test]
async fn registry_evicts_finished_entries_past_ttl() {
    let registry = registry(8, 0);
    let done = registry
        .start(ChatId(10), UserId(20), "завершённый", 2)
        .await
        .unwrap();
    done.finish(ResearchStatus::Completed, 2, 1).await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    let running = registry
        .start(ChatId(10), UserId(21), "активный", 2)
        .await
        .unwrap();

    let (rows, _) = registry.snapshots(chat_filter(10)).await;
    assert_eq!(
        rows.len(),
        1,
        "only the running pass survives the TTL sweep"
    );
    assert_eq!(rows[0].id, running.id());
    running.finish(ResearchStatus::Completed, 2, 1).await;
}

#[tokio::test]
async fn registry_cap_drops_oldest_finished_and_refuses_all_running() {
    let registry = registry(2, 60);
    let oldest = registry
        .start(ChatId(10), UserId(20), "старый", 2)
        .await
        .unwrap();
    let second = registry
        .start(ChatId(10), UserId(21), "второй", 2)
        .await
        .unwrap();

    let refused = registry.start(ChatId(10), UserId(22), "третий", 2).await;
    let Err(reason) = refused else {
        panic!("a third running pass must be refused");
    };
    assert_eq!(reason, "слишком много активных ресерчей");

    oldest.finish(ResearchStatus::Completed, 2, 1).await;
    let third = registry
        .start(ChatId(10), UserId(22), "третий", 2)
        .await
        .unwrap();

    let (rows, _) = registry.snapshots(chat_filter(10)).await;
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, vec![third.id(), second.id()]);
    second.finish(ResearchStatus::Completed, 2, 1).await;
    third.finish(ResearchStatus::Completed, 2, 1).await;
}

#[tokio::test]
async fn registry_bounds_snapshot_rows() {
    let registry = registry(32, 60);
    for index in 0..21 {
        let job = registry
            .start(ChatId(10), UserId(20), &format!("вопрос {index}"), 2)
            .await
            .unwrap();
        job.finish(ResearchStatus::Completed, 2, 1).await;
    }
    let (rows, truncated) = registry.snapshots(chat_filter(10)).await;
    assert_eq!(rows.len(), 20);
    assert!(truncated);
    assert_eq!(rows[0].question, "вопрос 20", "newest row first");
}

#[tokio::test]
async fn registry_no_id_cancel_targets_the_callers_own_pass() {
    let registry = registry(8, 60);
    let own = registry
        .start(ChatId(10), UserId(20), "свой", 2)
        .await
        .unwrap();
    let foreign = registry
        .start(ChatId(10), UserId(21), "чужой", 2)
        .await
        .unwrap();

    let canceled = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(20),
            owner: false,
            research_id: None,
        })
        .await
        .unwrap();
    assert_eq!(canceled.id, own.id());
    let (rows, _) = registry.snapshots(chat_filter(10)).await;
    let running: Vec<&str> = rows
        .iter()
        .filter(|row| row.status == ResearchStatus::Running)
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(running, vec![foreign.id()]);

    let by_owner = registry
        .cancel(CancelRequest {
            chat_id: ChatId(10),
            user_id: UserId(99),
            owner: true,
            research_id: None,
        })
        .await
        .unwrap();
    assert_eq!(by_owner.id, foreign.id());
    own.finish(ResearchStatus::Canceled, 1, 1).await;
    foreign.finish(ResearchStatus::Canceled, 1, 1).await;
}
