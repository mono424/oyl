use std::fs;

use super::*;

const MESSY: &str = include_str!("../../tests/fixtures/messy.bib");

fn open_text(text: &str) -> (tempfile::TempDir, Bibliography) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    fs::write(&path, text).unwrap();
    let bib = Bibliography::open(&path).unwrap();
    (dir, bib)
}

#[test]
fn missing_file_is_empty_and_save_without_changes_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    let mut bib = Bibliography::open(&path).unwrap();
    assert!(bib.is_empty());
    bib.save().unwrap();
    assert!(!path.exists());
}

#[test]
fn reads_a_messy_file() {
    let (_d, bib) = open_text(MESSY);
    let keys: Vec<&str> = bib.keys().collect();
    assert_eq!(
        keys,
        vec![
            "vaswani2017",
            "Goedel1931",
            "knuth1984",
            "he2016",
            "devlin2019bert"
        ]
    );
    assert_eq!(bib.problems().len(), 1);
    assert_eq!(bib.problems()[0].line, 38);

    let v = bib.get("vaswani2017").unwrap();
    assert_eq!(v.kind, "inproceedings");
    assert_eq!(
        v.venue.as_deref(),
        Some("Advances in Neural Information Processing Systems 30")
    );
    assert_eq!(v.authors.len(), 8);
    assert_eq!(v.authors[6], "Łukasz Kaiser");
    assert_eq!(v.year, Some(2017));
    assert_eq!(v.field("pages"), Some("5998--6008"));
    assert_eq!(v.candidate().authors, "Vaswani et al.");

    let g = bib.get("goedel1931").unwrap();
    assert_eq!(g.key, "Goedel1931");
    assert_eq!(g.kind, "article");
    assert_eq!(g.authors, vec!["Kurt Gödel"]);
    assert_eq!(
        g.title,
        "Über formal unentscheidbare Sätze der Principia Mathematica und verwandter Systeme I"
    );
    assert_eq!(
        g.venue.as_deref(),
        Some("Monatshefte für Mathematik und Physik")
    );
    assert_eq!(g.field("month"), Some("dec"));
    assert_eq!(g.field("volume"), Some("38"));

    let k = bib.get("knuth1984").unwrap();
    assert_eq!(k.title, "The TeXbook");
    assert_eq!(k.venue.as_deref(), Some("Addison-Wesley"));
    assert_eq!(k.authors, vec!["Donald E. Knuth"]);

    let h = bib.get("he2016").unwrap();
    assert_eq!(h.doi.as_deref(), Some("10.1109/CVPR.2016.90"));
    assert_eq!(
        h.venue.as_deref(),
        Some("IEEE Transactions on Pattern Analysis and Machine Intelligence")
    );
    assert_eq!(h.candidate().authors, "He et al.");

    let d = bib.get("devlin2019bert").unwrap();
    assert_eq!(d.year, Some(2018));
    assert_eq!(
        d.title,
        "BERT: Pre-training of Deep Bidirectional Transformers for Language Understanding"
    );
    assert_eq!(d.arxiv_id().as_deref(), Some("1810.04805"));
}

#[test]
fn untouched_file_renders_byte_for_byte() {
    let (_d, bib) = open_text(MESSY);
    assert_eq!(bib.render(), MESSY);
}

#[test]
fn adding_one_entry_leaves_the_rest_alone() {
    let (_d, mut bib) = open_text(MESSY);
    let key = bib.add(Source {
        kind: "article".into(),
        title: "Deep learning".into(),
        authors: vec![
            "Yann LeCun".into(),
            "Yoshua Bengio".into(),
            "Geoffrey Hinton".into(),
        ],
        year: Some(2015),
        venue: Some("Nature".into()),
        doi: Some("10.1038/nature14539".into()),
        ..Source::default()
    });
    assert_eq!(key, "lecun2015");
    let out = bib.render();
    assert!(
        out.starts_with(MESSY),
        "the existing text must be kept as it is"
    );
    assert_eq!(
        &out[MESSY.len()..],
        "\n@article{lecun2015,\n  title = {Deep learning},\n  author = {LeCun, Yann and Bengio, Yoshua and Hinton, Geoffrey},\n  year = {2015},\n  journal = {Nature},\n  doi = {10.1038/nature14539}\n}\n"
    );
}

#[test]
fn colliding_keys_get_letters() {
    let (_d, mut bib) = open_text(MESSY);
    let s = Source {
        title: "Another one".into(),
        authors: vec!["Someone Vaswani".into()],
        year: Some(2017),
        ..Source::default()
    };
    assert_eq!(bib.add(s.clone()), "vaswani2017b");
    assert_eq!(bib.add(s.clone()), "vaswani2017c");
    let mut explicit = s;
    explicit.key = "he2016".into();
    assert_eq!(bib.add(explicit), "he2016b");
}

#[test]
fn editing_a_field_changes_only_that_value() {
    let (_d, mut bib) = open_text(MESSY);
    let mut v = bib.get("vaswani2017").unwrap().clone();
    v.year = Some(2018);
    v.set_field("pages", "1--11");
    bib.update("vaswani2017", v).unwrap();
    let out = bib.render();
    let expected = MESSY
        .replace("  pages     = {5998--6008},", "  pages     = {1--11},")
        .replace("  year      = {2017},", "  year      = {2018},");
    assert_eq!(out, expected);
}

#[test]
fn editing_keeps_quotes_numbers_and_accents_that_did_not_change() {
    let (_d, mut bib) = open_text(MESSY);
    let mut g = bib.get("Goedel1931").unwrap().clone();
    g.year = Some(1932);
    g.set_field("volume", "39");
    bib.update("Goedel1931", g).unwrap();
    let out = bib.render();
    let expected = MESSY
        .replace("\tVolume = 38,", "\tVolume = 39,")
        .replace("\tYear = 1931,", "\tYear = 1932,");
    assert_eq!(out, expected);

    let mut k = bib.get("knuth1984").unwrap().clone();
    k.title = "The METAFONTbook".into();
    bib.update("knuth1984", k).unwrap();
    assert!(bib.render().contains("  title = \"The {METAFONTbook}\",\n"));
}

#[test]
fn adding_and_removing_fields_follow_the_entry_layout() {
    let (_d, mut bib) = open_text(MESSY);
    let mut v = bib.get("vaswani2017").unwrap().clone();
    v.doi = Some("10.5555/3295222.3295349".into());
    v.remove_field("pages");
    bib.update("vaswani2017", v).unwrap();
    let out = bib.render();
    let expected = MESSY.replace(
        "  pages     = {5998--6008},\n  year      = {2017},\n",
        "  year      = {2017},\n  doi       = {10.5555/3295222.3295349},\n",
    );
    assert_eq!(out, expected);

    // Tab-indented entry without a trailing comma, capitalized names.
    let mut g = bib.get("Goedel1931").unwrap().clone();
    g.url = Some("https://doi.org/10.1007/BF01700692".into());
    bib.update("Goedel1931", g).unwrap();
    assert!(
        bib.render()
            .contains("\tMonth = dec,\n\tUrl = {https://doi.org/10.1007/BF01700692}}")
    );

    // One-line entry.
    let mut h = bib.get("he2016").unwrap().clone();
    h.set_field("pages", "770--778");
    bib.update("he2016", h).unwrap();
    assert!(
        bib.render()
            .contains("doi={10.1109/CVPR.2016.90}, pages={770--778}}")
    );
}

#[test]
fn a_year_from_date_is_changed_in_date() {
    let (_d, mut bib) = open_text(MESSY);
    let mut d = bib.get("devlin2019bert").unwrap().clone();
    d.year = Some(2019);
    bib.update("devlin2019bert", d).unwrap();
    let out = bib.render();
    assert_eq!(
        out,
        MESSY.replace("date = {2018-10-11}", "date = {2019-10-11}")
    );
}

#[test]
fn renaming_a_key_and_changing_the_kind() {
    let (_d, mut bib) = open_text(MESSY);
    let mut k = bib.get("knuth1984").unwrap().clone();
    k.key = "knuth1984texbook".into();
    k.kind = "manual".into();
    assert_eq!(bib.update("knuth1984", k).unwrap(), "knuth1984texbook");
    assert!(bib.render().contains("@manual(knuth1984texbook,\n"));
    assert!(bib.contains("knuth1984texbook"));
    assert!(!bib.contains("knuth1984"));

    let mut taken = bib.get("he2016").unwrap().clone();
    taken.key = "vaswani2017".into();
    assert!(matches!(
        bib.update("he2016", taken),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        bib.update("nope", Source::default()),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn removing_an_entry_keeps_its_neighbours() {
    let (_d, mut bib) = open_text(MESSY);
    let removed = bib.remove("knuth1984").unwrap();
    assert_eq!(removed.title, "The TeXbook");
    let out = bib.render();
    let start = MESSY.find("@book(knuth1984").unwrap();
    let end = MESSY.find("@article{he2016").unwrap();
    let expected = format!("{}{}", &MESSY[..start], &MESSY[end..]);
    assert_eq!(out, expected);
    assert_eq!(bib.len(), 4);
}

#[test]
fn removing_the_last_entry_trims_the_end() {
    let (_d, mut bib) = open_text("@misc{a, title={A}}\n\n@misc{b, title={B}}\n");
    bib.remove("b");
    assert_eq!(bib.render(), "@misc{a, title={A}}\n");
}

#[test]
fn import_pasted_bibtex() {
    let (_d, mut bib) = open_text(MESSY);
    let pasted = r#"
@string{nat = "Nature"}
@article{lecun2015deep,
  title={Deep learning},
  author={LeCun, Yann and Bengio, Yoshua and Hinton, Geoffrey},
  journal=nat,
  volume={521},
  year={2015}
}
@inproceedings{he2016deep,
  title={Deep residual learning for image recognition},
  author={He, Kaiming and Zhang, Xiangyu and Ren, Shaoqing and Sun, Jian},
  booktitle={CVPR},
  year={2016},
  doi = {10.1109/cvpr.2016.90}
}
@misc{vaswani2017,
  title = {M{\"u}ller's {GPU} notes},
  author = {M{\"u}ller, J{\"o}rg},
  year = 2017
}"#;
    let keys = bib.import_bibtex(pasted).unwrap();
    assert_eq!(keys, vec!["lecun2015deep", "he2016", "vaswani2017b"]);
    let lecun = bib.get("lecun2015deep").unwrap();
    assert_eq!(lecun.venue.as_deref(), Some("Nature"));
    let m = bib.get("vaswani2017b").unwrap();
    assert_eq!(m.authors, vec!["Jörg Müller"]);
    let out = bib.render();
    assert!(out.starts_with(MESSY));
    // Written as pasted, with the new key; the macro-using entry rewritten.
    assert!(out.contains("@misc{vaswani2017b,\n  title = {M{\\\"u}ller's {GPU} notes},\n  author = {M{\\\"u}ller, J{\\\"o}rg},\n  year = 2017\n}"));
    assert!(out.contains("@article{lecun2015deep,\n  title = {Deep learning},"));
    assert!(out.contains("  journal = {Nature},"));
    assert!(!out.contains("journal=nat"));

    assert!(matches!(
        bib.import_bibtex("no bibtex here"),
        Err(Error::Parse(_))
    ));
}

#[test]
fn save_is_atomic_and_rereads() {
    let (dir, mut bib) = open_text(MESSY);
    bib.add(Source::new("misc", "A note"));
    assert!(bib.is_dirty());
    bib.save().unwrap();
    assert!(!bib.is_dirty());
    let on_disk = fs::read_to_string(dir.path().join("refs.bib")).unwrap();
    assert!(on_disk.starts_with(MESSY));
    assert!(on_disk.ends_with("@misc{note,\n  title = {A note}\n}\n"));
    let reread = Bibliography::open(dir.path().join("refs.bib")).unwrap();
    assert_eq!(reread.len(), 6);
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "no temporary files left: {names:?}");
}

#[test]
fn save_merges_with_changes_made_on_disk_meanwhile() {
    let (dir, mut bib) = open_text("@misc{a, title={A}}\n\n@misc{b, title={B}}\n");
    let path = dir.path().join("refs.bib");
    // Oyl adds one and edits one…
    bib.add(Source::new("misc", "Mine"));
    let mut b = bib.get("b").unwrap().clone();
    b.title = "B2".into();
    bib.update("b", b).unwrap();
    // …while an agent rewrote the file.
    fs::write(
        &path,
        "@misc{a, title={A}}\n\n@misc{b, title={B}}\n\n@misc{c, title={Agent's}}\n",
    )
    .unwrap();
    bib.save().unwrap();
    let out = fs::read_to_string(&path).unwrap();
    assert_eq!(
        out,
        "@misc{a, title={A}}\n\n@misc{b, title={B2}}\n\n@misc{c, title={Agent's}}\n\n@misc{mine,\n  title = {Mine}\n}\n"
    );
}

#[test]
fn latin1_files_stay_latin1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    let mut bytes = b"@misc{m, author = {M".to_vec();
    bytes.push(0xFC); // ü in Latin-1
    bytes.extend_from_slice(b"ller, J.}, title = {T}}\n");
    fs::write(&path, &bytes).unwrap();
    let mut bib = Bibliography::open(&path).unwrap();
    assert_eq!(bib.get("m").unwrap().authors, vec!["J. Müller"]);
    bib.add(Source::new("misc", "Zweite"));
    bib.save().unwrap();
    let written = fs::read(&path).unwrap();
    assert!(written.starts_with(&bytes));
}

#[test]
fn duplicates_are_found() {
    let (_d, bib) = open_text(MESSY);
    let by_doi = Source {
        doi: Some("https://doi.org/10.1109/cvpr.2016.90".into()),
        ..Source::default()
    };
    assert_eq!(bib.find_duplicate(&by_doi), Some("he2016"));
    let by_arxiv = Source {
        url: Some("https://arxiv.org/abs/1810.04805v2".into()),
        ..Source::default()
    };
    assert_eq!(bib.find_duplicate(&by_arxiv), Some("devlin2019bert"));
    let by_title = Source {
        title: "attention is all you need".into(),
        year: Some(2017),
        ..Source::default()
    };
    assert_eq!(bib.find_duplicate(&by_title), Some("vaswani2017"));
    assert_eq!(
        bib.find_duplicate(&Source::new("misc", "Something else")),
        None
    );
}

#[test]
fn candidates_and_bylines() {
    let (_d, bib) = open_text(MESSY);
    let c = bib.candidates();
    assert_eq!(c.len(), 5);
    assert_eq!(c[0].key, "vaswani2017");
    assert_eq!(c[0].authors, "Vaswani et al.");
    assert_eq!(c[0].year, Some(2017));
    assert_eq!(c[1].authors, "Gödel");
    let two = Source {
        authors: vec!["Ashish Vaswani".into(), "Noam Shazeer".into()],
        year: Some(2017),
        venue: Some("NeurIPS".into()),
        ..Source::default()
    };
    assert_eq!(two.byline(), "Vaswani and Shazeer · 2017 · NeurIPS");
    let edited = Source {
        fields: vec![("editor".into(), "Smith, Jane".into())],
        ..Source::default()
    };
    assert_eq!(edited.short_authors(), "Smith");
}

#[test]
fn round_trips_its_own_output() {
    let s = Source {
        key: "x".into(),
        kind: "article".into(),
        title: "R&D for {GPUs}: 100% of $5_000".into(),
        authors: vec!["Jörg Müller".into(), "et al.".into()],
        year: Some(2020),
        venue: Some("Journal of A & B".into()),
        doi: Some("10.1/abc_def".into()),
        url: Some("https://example.org/a_b?c=d#e".into()),
        abstract_text: Some("We do things.".into()),
        fields: vec![("note".into(), "\\emph{Accepted}".into())],
    };
    let (_d, bib) = open_text(&s.to_bibtex());
    let back = bib.get("x").unwrap();
    assert_eq!(back, &s);
}
