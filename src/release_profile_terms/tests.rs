use super::*;
fn terms(required: &[&str], ignored: &[&str]) -> Vec<ProfileTerms> {
    vec![ProfileTerms {
        required: required.iter().map(|s| (*s).to_owned()).collect(),
        ignored: ignored.iter().map(|s| (*s).to_owned()).collect(),
    }]
}
fn matched(term: &str, title: &str) -> Result<bool> {
    let prepared = prepare(MediaDomain::Tv, terms(&[term], &[]))?;
    Ok(evaluate(&prepared, title)?.profiles[0].required == vec![0])
}
#[test]
fn source_derived_recognizer_and_modifier_corpus() {
    // Requirements independently specified from pinned factories; not upstream implementation.
    for (term, title, expected) in [
        ("WEB", "movie.web.1080p", true),
        ("/WEB/", "movie.web.1080p", false),
        ("/WEB/i", "movie.web.1080p", true),
        ("prefix /WEB/i suffix", "web", true),
        ("/WEB/I", "web", false),
        ("/^B$/", "B\n", true),
        ("/^B$/", "B\n\n", false),
        (r"/^B\Z/", "B", true),
        (r"/^B\Z/", "B\n", true),
        (r"/^B\Z/", "B\n\n", false),
        (r"/^B\Z/m", "B\n\n", false),
        (r"/^(?m:B$)/", "B\n\n", true),
        (r"/^(?-m:B$)/m", "B\n\n", false),
        (r"/\cJ/", "\n", true),
        (r"/[\cj]/", "\n", true),
        (r"/\c[/", "\u{1b}", true),
        (r"/[\c[]/", "\u{1b}", true),
        (r"/\</", "<", true),
        (r"/\>/", ">", true),
        (r"/[\<\>]/", ">", true),
        (r"/\0/", "\0", true),
        (r"/[\1]/", "\u{1}", true),
        ("/[[]/", "[", true),
        ("/[a&&b]/", "a", true),
        (r"/[\b]/", "\u{8}", true),
        ("/WEB/ii", "web", true),
        ("//", "", true),
        ("/WEB", "a/web-z", true),
        ("/A/B/i", "a/b", true),
        ("/a\nb/", "/a\nb/", true),
        ("/a\nb/", "a\nb", false),
        ("/^B$/m", "A\nB\nC", true),
        ("/^B$/", "A\nB\nC", false),
        ("/A.B/s", "A\nB", true),
        ("/A.B/", "A\nB", false),
        ("/A B/x", "AB", true),
        ("/A B/x", "A B", false),
        ("/[()]/n", "(", true),
        (r"/a\/b/i", "A/B", true),
        ("junk\n/a/i trailing", "A", true),
        ("/a/ suffix /b/i", "a/ suffix /b", true),
        (r"/\bWEB\b/", "WEB-DL", true),
        ("/\u{8}WEB\u{8}/", "WEB-DL", false),
        (".WEB", "aXWEB", false),
        (".WEB", "a.web", true),
    ] {
        assert_eq!(matched(term, title), Ok(expected), "{term:?} / {title:?}");
    }
    for term in ["/WEB/ig", "/[/", r"/(A)\1/n"] {
        assert_eq!(matched(term, "AA"), Err(TermError::InvalidSyntax), "{term}");
    }
}
#[test]
fn explicit_capture_numbering_names_backrefs_and_scopes() {
    for (term, title, expected) in [
        (r"/^(?<word>A)(B)\1\2$/", "ABBA", true),
        (r"/^(?<word>A)(B)\1\2$/", "ABAB", false),
        (r"/^(?<word>A)(B)\k<word>$/", "ABA", true),
        (r"/^(?'word'A)(B)\k'word'$/n", "ABA", true),
        (r"/^(?<word>A)(B)\1$/n", "ABA", true),
        (r"/^(?<g1>A)(?<g2>B)\k<g1>\k<g2>$/n", "ABAB", true),
        (r"/^\1(A)$/", "AA", false), // Forward references compile, but an unset group cannot match.
        (r"/^(?(1)A|B)(C)$/", "BC", true),
        (r"/^(?<word>A)?(?(word)B|C)$/n", "AB", true),
        (r"/^(?<word>A)?(?(word)B|C)$/n", "C", true),
        (r"/^(?(?=A)A|B)$/n", "A", true),
        (r"/^(?(?=A)A|B)$/n", "B", true),
        (r"/^(?n:(A))(?-n:(B))\1$/", "ABB", true),
        (r"/^(?n:(A))(B)\1$/", "ABB", true),
        (r"/^(?-n:(A))(B)\1$/n", "ABA", true),
        (r"/^(?n)(A)(?-n)(B)\1$/", "ABB", true),
        (r"/^\((A)\)\1$/", "(A)A", true),
        (r"/^[()](?<a>A)\k<a>$/n", "(AA", true),
        (r"/^(?# fake ( unnamed)(A)\1$/", "AA", true),
        ("/^A [ #] B$/x", "A B", true),
        ("/^A [ #] B$/x", "A#B", true),
        ("/^A (?x:B C) D$/", "A BC D", true),
        ("/^A(?-x: B )C$/x", "A B C", true),
        ("/^A # (fake)\n (?<real>B)\\1$/nx", "ABB", false), // Outer delimiter cannot span newline: literal.
    ] {
        assert_eq!(matched(term, title), Ok(expected), "{term:?} / {title:?}");
    }
    assert_eq!(
        dialect::normalize("A # (fake)\n (?<real>B)\\1", "nx").unwrap(),
        r"A(B)\k<1>"
    );
    for term in [
        r"/(A)\1/n",
        r"/(?<name>A)\k<missing>/",
        r"/(A)\2/",
        "/(unclosed/",
    ] {
        assert_eq!(matched(term, "AA"), Err(TermError::InvalidSyntax), "{term}");
    }
    for term in [
        r"/(?<x>A)(?<x>B)\k<x>/",
        r"/(?<2>A)\2/",
        r"/(?<x-y>A)/",
        r"/[a-z-[ae]]/",
        r"/\p{L}/",
        r"/\P{IsGreek}/",
        r"/[\p{L}]/",
        r"/[\P{L}]/",
        r"/[\R]/",
        r"/\u{40}/",
        r"/\uD800/",
        r"/\g<0>/",
        r"/(A)\10/",
    ] {
        assert_eq!(
            matched(term, "AA"),
            Err(TermError::UnsupportedDialect),
            "{term}"
        );
    }
}
#[test]
fn literal_unicode_baseline_is_not_regex_culture_equivalence() {
    // Native simple invariant lowering and engine Unicode behavior are explicit;
    // these are not evidence of every deployment's .NET CurrentCulture behavior.
    for (term, title, expected) in [
        ("I", "i", true),
        ("I", "ı", false),
        ("İ", "i", false),
        ("Σ", "σ", true),
        ("Σ", "ς", false),
        ("K", "k", true),
        ("é", "e\u{301}", false),
        ("/Σ/i", "ς", true),
        ("/k/i", "K", true),
        ("/I/i", "ı", false),
    ] {
        assert_eq!(matched(term, title), Ok(expected), "{term:?} / {title:?}");
    }
}
#[test]
fn bounds_are_whole_catalog_and_never_partial_results() {
    assert_eq!(
        prepare(MediaDomain::Tv, terms(&[" "], &[])).unwrap_err(),
        TermError::InvalidSyntax
    );
    assert_eq!(
        prepare(
            MediaDomain::Tv,
            terms(&[&"x".repeat(MAX_TERM_BYTES + 1)], &[])
        )
        .unwrap_err(),
        TermError::LimitExceeded
    );
    let many = vec![
        ProfileTerms {
            required: vec!["a".into(); 200],
            ignored: vec![]
        };
        21
    ];
    assert_eq!(
        prepare(MediaDomain::Tv, many).unwrap_err(),
        TermError::LimitExceeded
    );
    let bytes = vec![
        ProfileTerms {
            required: vec!["a".repeat(2048); 200],
            ignored: vec![]
        };
        3
    ];
    assert_eq!(
        prepare(MediaDomain::Tv, bytes).unwrap_err(),
        TermError::LimitExceeded
    );
    assert_eq!(
        matched("/((?=a)){1000000000}/", "a"),
        Err(TermError::LimitExceeded)
    );
    assert_eq!(
        matched(&format!("/{}a{}/", "(".repeat(65), ")".repeat(65)), "a"),
        Err(TermError::LimitExceeded)
    );
    let expensive = vec![
        ProfileTerms {
            required: vec!["/a{4096}/".into(); 200],
            ignored: vec![]
        };
        2
    ];
    assert_eq!(
        prepare(MediaDomain::Tv, expensive).unwrap_err(),
        TermError::LimitExceeded
    );
    let prepared = prepare(MediaDomain::Tv, terms(&["a"], &["z"])).unwrap();
    assert_eq!(
        evaluate(&prepared, &"a".repeat(MAX_TITLE_BYTES + 1)),
        Err(TermError::LimitExceeded)
    );
    assert_eq!(
        prepare(MediaDomain::Tv, terms(&["a"], &["/[/"])).unwrap_err(),
        TermError::InvalidSyntax
    );
}
#[tokio::test]
async fn worker_cache_identity_and_cancellation_are_bounded() {
    // One async test owns global admission tests; synchronous corpus tests don't share permits.
    let input = terms(&["CACHE-UNIQUE-a", "a", "a"], &["z"]);
    assert_eq!(
        cached_terms(MediaDomain::Tv, &input, "a"),
        Err(TermError::StateChanged)
    );
    let prepared = prepare_terms(MediaDomain::Tv, input.clone()).await.unwrap();
    let result = evaluate_terms(&prepared, "a").await.unwrap();
    assert_eq!(result.profiles[0].required, vec![1, 2]);
    assert!(result.profiles[0].ignored.is_empty());
    assert_eq!(cached_terms(MediaDomain::Tv, &input, "a"), Ok(result));
    assert_eq!(
        cached_terms(MediaDomain::Movies, &input, "a"),
        Err(TermError::StateChanged)
    );
    assert_eq!(
        cached_terms(MediaDomain::Tv, &input, "A"),
        Err(TermError::StateChanged)
    );
    let mut changed = input.clone();
    changed[0].required.swap(0, 1);
    assert_eq!(
        validate_prepared(&prepared, MediaDomain::Tv, &changed),
        Err(TermError::StateChanged)
    );
    assert_eq!(
        validate_prepared(&prepared, MediaDomain::Movies, &input),
        Err(TermError::StateChanged)
    );
    assert_eq!(
        validate_prepared(&prepared, MediaDomain::Tv, &input),
        Ok(())
    );
    assert!(
        evaluate_terms(&prepared, "zzzz-unique")
            .await
            .unwrap()
            .profiles[0]
            .required
            .is_empty()
    );
    assert!(
        cached_terms(MediaDomain::Tv, &input, "zzzz-unique")
            .unwrap()
            .profiles[0]
            .required
            .is_empty()
    );
    let cpu = CPU.get().unwrap().clone();
    let spare = cpu.clone().try_acquire_owned().unwrap();
    let (began_tx, began_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let task = tokio::spawn(bounded(move || {
        began_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(())
    }));
    began_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(cpu.available_permits(), 0);
    assert_eq!(
        prepare_terms(MediaDomain::Tv, input.clone())
            .await
            .unwrap_err(),
        TermError::Busy
    );
    release_tx.send(()).unwrap();
    // Acquiring waits for the actual closure's RAII release, not caller cancellation.
    let returned = tokio::time::timeout(Duration::from_secs(2), cpu.clone().acquire_owned())
        .await
        .unwrap()
        .unwrap();
    drop(returned);
    // Event synchronization proves the closure is live when the short caller
    // deadline expires. Production still uses the unchanged ten-second deadline.
    let (began_tx, began_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let task = tokio::spawn(bounded_with_timeout(
        move || {
            began_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        },
        Duration::from_millis(10),
    ));
    began_rx.await.unwrap();
    assert_eq!(task.await.unwrap(), Err(TermError::Timeout));
    assert_eq!(cpu.available_permits(), 0);
    assert_eq!(
        prepare_terms(MediaDomain::Tv, input.clone())
            .await
            .unwrap_err(),
        TermError::Busy
    );
    release_tx.send(()).unwrap();
    let returned = tokio::time::timeout(Duration::from_secs(2), cpu.clone().acquire_owned())
        .await
        .unwrap()
        .unwrap();
    drop(returned);
    drop(spare);
    let panic = bounded::<()>(|| panic!("owned test panic")).await;
    assert_eq!(panic, Err(TermError::WorkerFailed));
    for n in 0..=CACHE_ENTRIES {
        evaluate_terms(&prepared, &format!("cache-{n}"))
            .await
            .unwrap();
    }
    assert_eq!(
        cached_terms(MediaDomain::Tv, &input, "a"),
        Err(TermError::StateChanged)
    );
    let cache = CACHE.get().unwrap().lock().unwrap();
    assert!(cache.entries.len() <= CACHE_ENTRIES && cache.bytes <= CACHE_BYTES);
}
