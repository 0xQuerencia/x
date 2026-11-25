//! Native microphone stream callbacks: mono samples, capture timing and delivery.

use std::sync::mpsc::Sender;

use color_eyre::eyre::{Result, WrapErr};
use cpal::traits::DeviceTrait;
use cpal::{Device, SizedSample, Stream, StreamConfig};

use super::{CaptureInstant, StreamErrorReporter, duration_for_frames};

fn mono<T>(samples: &[T], channels: usize, convert: impl Fn(&T) -> f32) -> Vec<f32> {
    samples
        .chunks(channels)
        .map(|frame| frame.iter().map(&convert).sum::<f32>() / channels as f32)
        .collect()
}

pub(super) fn build_stream<T: SizedSample>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    sender: Sender<(Vec<f32>, CaptureInstant)>,
    reporter: StreamErrorReporter,
    mut feed: crate::audio::hex_microphone::CaptureFeed,
    convert: impl Fn(T) -> f32 + Send + 'static,
) -> Result<Stream> {
    let sample_rate = config.sample_rate;
    device
        .build_input_stream(
            *config,
            move |data: &[T], info| {
                let captured_through = captured_through(info, data.len() / channels, sample_rate);
                let mono_samples = mono(data, channels, |sample| convert(*sample));
                feed.push(&mono_samples);
                let _ = sender.send((mono_samples, captured_through));
            },
            move |error| reporter.report(error),
            None,
        )
        .wrap_err_with(|| format!("could not open {} microphone stream", T::FORMAT))
}

fn captured_through(
    info: &cpal::InputCallbackInfo,
    frames: usize,
    sample_rate: u32,
) -> CaptureInstant {
    let captured_at = CaptureInstant::from_stream(info.timestamp().capture);
    let duration = duration_for_frames(frames, sample_rate);
    captured_at.checked_add(duration).unwrap_or(captured_at)
}
