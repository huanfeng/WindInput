use super::*;

#[test]
fn report_overwrites_and_list_filters_by_root() {
    let a = Path::new("/tmp/issues-t1/data/a.dict.yaml");
    let b = Path::new("/tmp/issues-t1-other/b");
    report(a, IssueKind::BadLines(2));
    report(a, IssueKind::Isolated);
    report(b, IssueKind::Isolated);
    let v = list_under(Path::new("/tmp/issues-t1"));
    assert_eq!(
        v,
        vec![FileIssue {
            path: a.into(),
            kind: IssueKind::Isolated
        }]
    );
    clear(a);
    assert!(list_under(Path::new("/tmp/issues-t1")).is_empty());
    clear(b);
}

/// 只清「内容不合规」：同一文件的别的问题（已重建 / 隔离）留着。
#[test]
fn clear_invalid_keeps_other_kinds() {
    let a = Path::new("/tmp/issues-t3/a");
    report(
        a,
        IssueKind::Rebuilt {
            kept_as: "x".into(),
        },
    );
    clear_invalid(a);
    assert_eq!(
        list_under(a)[0].kind,
        IssueKind::Rebuilt {
            kept_as: "x".into()
        }
    );
    report(
        a,
        IssueKind::Invalid {
            message: "m".into(),
        },
    );
    clear_invalid(a);
    assert!(list_under(a).is_empty());
}

#[test]
fn report_parse_clears_when_clean() {
    let a = Path::new("/tmp/issues-t2/x");
    report_parse(a, 3);
    assert_eq!(list_under(a)[0].kind, IssueKind::BadLines(3));
    report_parse(a, 0);
    assert!(list_under(a).is_empty());
}
