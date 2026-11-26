use super::*;
use crate::store_commands::tests::listing;

const FIRST: &str = "https://first.example";
const SECOND: &str = "https://second.example";

fn catalog(version: &str) -> Metadata {
  let mut applet = listing("sample", version, None);
  applet.latest_release.release_notes = "Full notes".into();
  applet.latest_stable_release.as_mut().unwrap().release_notes = "Full notes".into();
  Metadata::Catalog(StoreCatalog {
    applets: vec![applet],
  })
}

fn detail(id: &str, version: &str) -> Metadata {
  let mut applet = listing(id, version, None);
  applet.latest_release.release_notes = "Full notes".into();
  applet.latest_stable_release.as_mut().unwrap().release_notes = "Full notes".into();
  Metadata::Detail(StoreDetail {
    releases: vec![applet.latest_release.clone()],
    applet,
  })
}

#[test]
fn newer_successful_metadata_wins_over_a_late_overlapping_response() {
  let root = tempfile::tempdir().unwrap();
  let cache = Arc::new(BrowseCache::default());
  let slow = cache.observe(FIRST, None).unwrap();
  cache
    .observe(FIRST, None)
    .unwrap()
    .publish(root.path(), catalog("2.0.0"));
  slow.publish(root.path(), catalog("1.0.0"));
  drop(slow);
  let older = cache.observe(FIRST, Some("sample")).unwrap();
  let failed = cache.observe(FIRST, Some("sample")).unwrap();
  drop(failed);
  older.publish(root.path(), detail("sample", "3.0.0"));
  drop(older);
  let snapshot = read_cache(root.path());
  let origin = &snapshot.origins[0];
  let applet = &origin.catalog.as_ref().unwrap().applets[0];
  assert_eq!(applet.latest_release.version, "2.0.0");
  assert!(applet.latest_release.release_notes.is_empty());
  assert_eq!(origin.details[0].releases[0].version, "3.0.0");
  assert_eq!(origin.details[0].releases[0].release_notes, "Full notes");
  assert!(cache.in_flight.lock().unwrap().is_empty());
}

#[test]
fn origin_and_detail_limits_keep_recent_public_snapshots_independently() {
  let root = tempfile::tempdir().unwrap();
  let cache = Arc::new(BrowseCache::default());
  for (origin, version) in [(FIRST, "1.0.0"), (SECOND, "2.0.0")] {
    cache
      .observe(origin, None)
      .unwrap()
      .publish(root.path(), catalog(version));
    cache
      .observe(origin, Some("sample"))
      .unwrap()
      .publish(root.path(), detail("sample", version));
  }
  let snapshot = read_cache(root.path());
  let [second, first] = &snapshot.origins[..] else {
    panic!("expected two saved origins")
  };
  assert_eq!(
    (&*second.store_url, &*second.details[0].releases[0].version),
    (SECOND, "2.0.0")
  );
  assert_eq!(
    (&*first.store_url, &*first.details[0].releases[0].version),
    (FIRST, "1.0.0")
  );
  for index in 0..MAX_DETAILS {
    let id = format!("applet-{index}");
    cache
      .observe(FIRST, Some(&id))
      .unwrap()
      .publish(root.path(), detail(&id, "1.0.0"));
  }
  let snapshot = read_cache(root.path());
  let details = &snapshot.origins[0].details;
  assert_eq!(details.len(), MAX_DETAILS);
  assert!(details
    .iter()
    .all(|detail| detail.applet.applet_id != "sample"));
  assert_eq!(snapshot.origins[1].details[0].applet.applet_id, "sample");
  for index in 0..MAX_ORIGINS {
    let origin = format!("https://store-{index}.example");
    cache
      .observe(&origin, None)
      .unwrap()
      .publish(root.path(), catalog("1.0.0"));
  }
  assert_eq!(read_cache(root.path()).origins.len(), MAX_ORIGINS);
  fs::write(root.path().join(CACHE_FILE), b"invalid cache").unwrap();
  assert!(read_cache(root.path()).origins.is_empty());
  fs::write(
    root.path().join(CACHE_FILE),
    vec![b'x'; MAX_CACHE_BYTES + 1],
  )
  .unwrap();
  assert!(read_cache(root.path()).origins.is_empty());
}
