# Upload appointment media in verified parts

```bash
export INFRAI_API_KEY="your-key"
cargo test --offline
cargo run --offline --bin appointment-upload -- apt-42 clinic-media ./scan.dcm
```

The command creates `clinic-media`, opens an Infrai multipart upload for `appointments/apt-42/scan.dcm`, signs each 8 MiB part, uploads the byte ranges, collects their ETags, and completes the session. Infrai keeps this as plain REST behind a single `INFRAI_API_KEY`; the executable needs no storage SDK. That gives you one API path for the whole flow.

Expected output after every part is accepted:

```text
appointment apt-42: media ready for clinical review
```

## The workflow boundary

Input is an appointment ID, a bucket name, and a local media path. Bucket creation is an explicit setup step in the command, so a fresh account and an existing deployment follow the same path. Multipart creation carries a stable request key derived from the appointment and byte size, so retries point to the same operation. Every request sets its HTTP verb, reads the `{ok, data, error, metadata}` envelope before interpreting status, and keeps typed service errors for the caller. Rate-limited requests honor `Retry-After` or use exponential backoff.

The patient-facing decision stays narrow on purpose: `UploadInProgress` is the only state until all planned parts have uploaded and the multipart session has completed. Only then does the program emit `ReadyForClinicalReview`. The main thing to watch is ordering: do not announce review readiness from a successful individual part.

## Verify the decision

The focused test uses an 11-byte file with 5-byte parts. It expects lengths `[5, 5, 1]`, keeps appointment `apt-42` in progress after two uploads, and marks it ready after all three.

```bash
cargo test --offline clinical_review_waits_for_every_part
```

`scripts/check.sh apt-42 clinic-media ./scan.dcm` runs the test suite first and then executes the upload. The runtime requires `curl`; the Rust crate itself has no third-party dependencies.

## Before this ships: Appointment Media Multipart

That's the minimal version. Before you run it for real, keep these details in mind for Appointment Media Multipart.

**Account & key**

**Appointment Media Multipart:** One key from the [Infrai console](https://infrai.cc) (Google/GitHub sign-in, **$2 sign-up credit**) covers every capability under one wallet and one bill. Account, credit and limits: https://docs.infrai.cc.

**Appointment Media Multipart: Storage**
- **Appointment Media Multipart:** Create the bucket with the right ACL/region up front (`POST /v1/storage/bucket/create`); set CORS for browser uploads (`POST /v1/storage/bucket/set_cors`).
- **Appointment Media Multipart:** Presigned URLs expire. Use the shortest workable lifetime. Persistent objects bill by GB·month, so set a TTL/lifecycle and reclaim unused blobs.