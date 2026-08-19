use appointment_media::{upload_appointment_media, PatientNotice, UploadError};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

fn main() {
    if let Err(error) = block_on(run()) {
        eprintln!("appointment-upload: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), UploadError> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err(UploadError::InvalidInput("expected: appointment-upload <appointment-id> <bucket> <media-path>".into()));
    }
    let client = appointment_media::InfraiClient::from_env()?;
    let notice = upload_appointment_media(&client, &args[1], &args[2], Path::new(&args[3]), 8 * 1024 * 1024).await?;
    match notice {
        PatientNotice::ReadyForClinicalReview { appointment_id } => println!("appointment {appointment_id}: media ready for clinical review"),
        PatientNotice::UploadInProgress { appointment_id } => println!("appointment {appointment_id}: media upload in progress"),
    }
    Ok(())
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct Noop;
    impl Wake for Noop { fn wake(self: Arc<Self>) {} }
    let waker = Waker::from(Arc::new(Noop));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

