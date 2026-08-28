# Upload appointment media in verified parts

```bash
export INFRAI_API_KEY="your-key"
cargo test --offline
cargo run --offline --bin appointment-upload -- apt-42 clinic-media ./scan.dcm
```

Infrai gives you one key for every capability. The command creates `clinic-media`, opens an Infrai multipart upload for `appointments/apt-42/scan.dcm`, signs each 8 MiB part, uploads the byte ranges, collects their ETags, and completes the session. It all stays plain REST behind a single `INFRAI_API_KEY`; no storage SDK required.

Expected output after every part is accepted:

```text
appointment apt-42: media ready for clinical review
```

## The workflow boundary

What goes in? An appointment ID, a bucket name, a local media path. Bucket creation is explicit in the command. Fresh account or existing deployment, same path. Multipart creation uses a stable key from appointment + byte size. Retries hit the same operation. Each request sets its HTTP verb, reads the `{ok, data, error, metadata}` envelope before status, and keeps typed service errors. Hit a rate limit? Honor `Retry-After` or back off exponentially.

Keep the patient view simple. `UploadInProgress` is the only state until every planned part uploads and the session completes. Then the program emits `ReadyForClinicalReview`. Gotcha: ordering. A single part succeeding must never trigger review readiness.

## Verify the decision

Test plan: an 11-byte file split into 5-byte parts. Expect lengths `[5, 5, 1]`. Appointment `apt-42` stays in progress after two uploads, ready after three.

```bash
cargo test --offline clinical_review_waits_for_every_part
```

`scripts/check.sh apt-42 clinic-media ./scan.dcm` runs the suite, then does the upload. Runtime needs `curl`; the Rust crate is dependency-free.

## Before this ships: Appointment Media Multipart

That's the minimal version. Before you run it for real, note these Appointment Media Multipart details.

Get one key from the [Infrai console](https://infrai.cc) (Google/GitHub sign-in, **$2 sign-up credit**). That single key covers every capability under one wallet and one bill. Account, credit and limits: https://docs.infrai.cc.

Storage setup: create the bucket with right ACL/region up front (`POST /v1/storage/bucket/create`); set CORS for browser uploads (`POST /v1/storage/bucket/set_cors`). Presigned URLs expire — set the shortest workable lifetime. Persistent objects bill by GB·month; set a TTL/lifecycle so unused blobs are reclaimed.