//! Pure software video decoding backend.

use crate::backend::render::{BitmapHandle, BitmapInfo, RenderBackend};
use crate::backend::video::{
    DecodedFrame, EncodedFrame, Error, FrameDependency, VideoBackend, VideoStreamHandle,
};
use generational_arena::Arena;
use swf::{VideoCodec, VideoDeblocking};

/// Software video backend that proxies to CPU-only codec implementations that
/// ship with Ruffle.
pub struct SoftwareVideoBackend {
    streams: Arena<VideoStream>,
}

impl Default for SoftwareVideoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftwareVideoBackend {
    pub fn new() -> Self {
        Self {
            streams: Arena::new(),
        }
    }
}

impl VideoBackend for SoftwareVideoBackend {
    #[allow(unreachable_code, unused_variables)]
    fn register_video_stream(
        &mut self,
        _num_frames: u32,
        size: (u16, u16),
        codec: VideoCodec,
        _filter: VideoDeblocking,
    ) -> Result<VideoStreamHandle, Error> {
        let decoder: Box<dyn VideoDecoder> = match codec {
            #[cfg(feature = "h263")]
            VideoCodec::H263 => Box::new(h263::H263Decoder::new()),
            #[cfg(feature = "vp6")]
            VideoCodec::Vp6 => Box::new(vp6::Vp6Decoder::new(false, size)),
            #[cfg(feature = "vp6")]
            VideoCodec::Vp6WithAlpha => Box::new(vp6::Vp6Decoder::new(true, size)),
            _ => return Err(format!("Unsupported video codec type {:?}", codec).into()),
        };
        let stream = VideoStream::new(decoder);
        let stream_handle = self.streams.insert(stream);
        Ok(stream_handle)
    }

    fn preload_video_stream_frame(
        &mut self,
        stream: VideoStreamHandle,
        encoded_frame: EncodedFrame<'_>,
    ) -> Result<FrameDependency, Error> {
        let stream = self
            .streams
            .get_mut(stream)
            .ok_or("Unregistered video stream")?;

        stream.decoder.preload_frame(encoded_frame)
    }

    fn decode_video_stream_frame(
        &mut self,
        stream: VideoStreamHandle,
        encoded_frame: EncodedFrame<'_>,
        renderer: &mut dyn RenderBackend,
    ) -> Result<BitmapInfo, Error> {
        let stream = self
            .streams
            .get_mut(stream)
            .ok_or("Unregistered video stream")?;

        let frame = stream.decoder.decode_frame(encoded_frame)?;
        let handle = if let Some(bitmap) = stream.bitmap {
            renderer.update_texture(bitmap, frame.width.into(), frame.height.into(), frame.rgba)?
        } else {
            renderer.register_bitmap_raw(frame.width.into(), frame.height.into(), frame.rgba)?
        };
        stream.bitmap = Some(handle);

        Ok(BitmapInfo {
            handle,
            width: frame.width,
            height: frame.height,
        })
    }
}

/// A single preloaded video stream.
struct VideoStream {
    bitmap: Option<BitmapHandle>,
    decoder: Box<dyn VideoDecoder>,
}

impl VideoStream {
    fn new(decoder: Box<dyn VideoDecoder>) -> Self {
        Self {
            decoder,
            bitmap: None,
        }
    }
}

/// Trait for video decoders.
/// This should be implemented for each video codec.
trait VideoDecoder {
    /// Preload a frame.
    ///
    /// No decoding is intended to happen at this point in time. Instead, the
    /// video data should be inspected to determine inter-frame dependencies
    /// between this and any previous frames in the stream.
    ///
    /// Frames should be preloaded in the order that they are recieved.
    ///
    /// Any dependencies listed here are inherent to the video bitstream. The
    /// containing video stream is also permitted to introduce additional
    /// interframe dependencies.
    fn preload_frame(&mut self, encoded_frame: EncodedFrame<'_>) -> Result<FrameDependency, Error>;

    /// Decode a frame of a given video stream.
    ///
    /// This function is provided the external index of the frame, the codec
    /// used to decode the data, and what codec to decode it with. The codec
    /// provided here must match the one used to register the video stream.
    ///
    /// Frames may be decoded in any order that does not violate the frame
    /// dependencies declared by the output of `preload_video_stream_frame`.
    ///
    /// The decoded frame should be returned. An `Error` can be returned if
    /// a drawable bitmap can not be produced.
    fn decode_frame(&mut self, encoded_frame: EncodedFrame<'_>) -> Result<DecodedFrame, Error>;
}

#[cfg(feature = "h263")]
mod h263 {
    use crate::backend::video::software::VideoDecoder;
    use crate::backend::video::{DecodedFrame, EncodedFrame, Error, FrameDependency};
    use h263_rs::parser::H263Reader;
    use h263_rs::{DecoderOption, H263State, PictureTypeCode};
    use h263_rs_yuv::bt601::yuv420_to_rgba;

    /// H263 video decoder.
    pub struct H263Decoder(H263State);

    impl H263Decoder {
        pub fn new() -> Self {
            Self(H263State::new(DecoderOption::SORENSON_SPARK_BITSTREAM))
        }
    }

    impl VideoDecoder for H263Decoder {
        fn preload_frame(
            &mut self,
            encoded_frame: EncodedFrame<'_>,
        ) -> Result<FrameDependency, Error> {
            let mut reader = H263Reader::from_source(encoded_frame.data());
            let picture = self
                .0
                .parse_picture(&mut reader, None)?
                .ok_or("Picture in video stream is not a picture")?;

            match picture.picture_type {
                PictureTypeCode::IFrame => Ok(FrameDependency::None),
                PictureTypeCode::PFrame => Ok(FrameDependency::Past),
                PictureTypeCode::DisposablePFrame => Ok(FrameDependency::Past),
                _ => Err("Invalid picture type code!".into()),
            }
        }

        fn decode_frame(&mut self, encoded_frame: EncodedFrame<'_>) -> Result<DecodedFrame, Error> {
            let mut reader = H263Reader::from_source(encoded_frame.data());

            self.0.decode_next_picture(&mut reader)?;

            let picture = self
                .0
                .get_last_picture()
                .expect("Decoding a picture should let us grab that picture");

            let (width, height) = picture
                .format()
                .into_width_and_height()
                .ok_or("H.263 decoder error!")?;
            let chroma_width = picture.chroma_samples_per_row();
            let (y, b, r) = picture.as_yuv();
            let rgba = yuv420_to_rgba(y, b, r, width.into(), chroma_width);
            Ok(DecodedFrame {
                width,
                height,
                rgba,
            })
        }
    }

    impl Default for H263Decoder {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(feature = "vp6")]
mod vp6 {
    use crate::backend::video::software::VideoDecoder;
    use crate::backend::video::{DecodedFrame, EncodedFrame, Error, FrameDependency};

    use h263_rs_yuv::bt601::yuv420_to_rgba;

    use nihav_codec_support::codecs::{NABufferRef, NAVideoBuffer, NAVideoInfo};
    use nihav_codec_support::codecs::{NABufferType::Video, YUV420_FORMAT};
    use nihav_core::codecs::NADecoderSupport;
    use nihav_duck::codecs::vp6::{VP56Decoder, VP56Parser, VP6BR};
    use nihav_duck::codecs::vpcommon::{BoolCoder, VP_YUVA420_FORMAT};

    /// VP6 video decoder.
    pub struct Vp6Decoder {
        with_alpha: bool,
        bounds: (u16, u16),
        decoder: VP56Decoder,
        support: NADecoderSupport,
        bitreader: VP6BR,
        init_called: bool,
        last_frame: Option<NABufferRef<NAVideoBuffer<u8>>>,
    }

    impl Vp6Decoder {
        pub fn new(with_alpha: bool, bounds: (u16, u16)) -> Self {
            // Unfortunately, `init()` cannot be called on the decoder
            // just yet, because `bounds` is only the declared size of
            // the video, to which it will be cropped.
            // This can be (rarely) even much smaller than the actual
            // encoded size of the frames.
            // `VP56Decoder::init()` needs the full encoded frame size,
            // so it can allocate its internal buffers accordingly.
            // The encoded frame size will be parsed from the header of
            // the first encoded frame passed to `Self::decode_frame()`.

            Self {
                with_alpha,
                bounds,
                decoder: VP56Decoder::new(6, with_alpha, true),
                support: NADecoderSupport::new(),
                bitreader: VP6BR::new(),
                init_called: false,
                last_frame: None,
            }
        }
    }

    impl VideoDecoder for Vp6Decoder {
        fn preload_frame(
            &mut self,
            encoded_frame: EncodedFrame<'_>,
        ) -> Result<FrameDependency, Error> {
            // Luckily the very first bit of the encoded frames is exactly this flag,
            // so we don't have to bother asking any "proper" decoder or parser.
            Ok(
                if !encoded_frame.data.is_empty() && (encoded_frame.data[0] & 0b_1000_0000) == 0 {
                    FrameDependency::None
                } else {
                    FrameDependency::Past
                },
            )
        }

        fn decode_frame(&mut self, encoded_frame: EncodedFrame<'_>) -> Result<DecodedFrame, Error> {
            // If this is the first frame, the decoder needs to be initialized.

            if !self.init_called {
                let mut bool_coder = BoolCoder::new(if self.with_alpha {
                    // The 24 bits alpha offset needs to be skipped first in this case
                    &encoded_frame.data[3..]
                } else {
                    encoded_frame.data
                })
                .map_err(|error| {
                    Error::from(format!("Error constructing VP6 bool coder: {:?}", error))
                })?;

                let header = self
                    .bitreader
                    .parse_header(&mut bool_coder)
                    .map_err(|error| {
                        Error::from(format!("Error parsing VP6 frame header: {:?}", error))
                    })?;

                let video_info = NAVideoInfo::new(
                    header.disp_w as usize * 16,
                    header.disp_h as usize * 16,
                    true,
                    if self.with_alpha {
                        VP_YUVA420_FORMAT
                    } else {
                        YUV420_FORMAT
                    },
                );

                self.decoder
                    .init(&mut self.support, video_info)
                    .map_err(|error| {
                        Error::from(format!("Error initializing VP6 decoder: {:?}", error))
                    })?;

                self.init_called = true;
            }

            let frame = if encoded_frame.data.is_empty()
                || (self.with_alpha && encoded_frame.data.len() <= 3)
            {
                // This frame is empty, so it's a "skip frame"; reusing the last frame, if there is one.

                match &self.last_frame {
                    Some(frame) => frame.clone(),
                    None => {
                        return Err(Error::from(
                            "No previous frame found when encountering a skip frame",
                        ))
                    }
                }
            } else {
                // Actually decoding the frame and extracting the buffer it is stored in.

                let decoded = self
                    .decoder
                    .decode_frame(&mut self.support, encoded_frame.data, &mut self.bitreader)
                    .map_err(|error| Error::from(format!("VP6 decoder error: {:?}", error)))?;

                let frame = match decoded {
                    (Video(buffer), _) => Ok(buffer),
                    _ => Err(Error::from(
                        "Unexpected buffer type after decoding a VP6 frame",
                    )),
                }?;

                self.last_frame = Some(frame.clone());

                frame
            };

            // Converting it from YUV420 to RGBA.

            let yuv = frame.get_data();

            let (mut width, mut height) = frame.get_dimensions(0);
            let (chroma_width, chroma_height) = frame.get_dimensions(1);

            // We assume that there is no padding between rows
            debug_assert!(frame.get_stride(0) == frame.get_dimensions(0).0);
            debug_assert!(frame.get_stride(1) == frame.get_dimensions(1).0);
            debug_assert!(frame.get_stride(2) == frame.get_dimensions(2).0);

            // Where each plane starts in the buffer
            let offsets = (
                frame.get_offset(0),
                frame.get_offset(1),
                frame.get_offset(2),
            );

            let mut rgba = yuv420_to_rgba(
                &yuv[offsets.0..offsets.0 + width * height],
                &yuv[offsets.1..offsets.1 + chroma_width * chroma_height],
                &yuv[offsets.2..offsets.2 + chroma_width * chroma_height],
                width,
                chroma_width,
            );

            // Adding in the alpha component, if present.

            if self.with_alpha {
                debug_assert!(frame.get_stride(3) == frame.get_dimensions(3).0);
                let alpha_offset = frame.get_offset(3);
                let alpha = &yuv[alpha_offset..alpha_offset + width * height];
                for (alpha, rgba) in alpha.iter().zip(rgba.chunks_mut(4)) {
                    // The SWF spec mandates the `min` to avoid any accidental "invalid"
                    // premultiplied colors, which would cause strange results after blending.
                    // And the alpha data is encoded in full range (0-255), unlike the Y
                    // component of the main color data, so no remapping is needed.
                    rgba.copy_from_slice(&[
                        u8::min(rgba[0], *alpha),
                        u8::min(rgba[1], *alpha),
                        u8::min(rgba[2], *alpha),
                        *alpha,
                    ]);
                }
            }

            // Cropping the encoded frame (containing whole macroblocks) to the
            // size requested by the the bounds attribute.

            let &bounds = &self.bounds;

            if width < bounds.0 as usize || height < bounds.1 as usize {
                log::warn!("A VP6 video frame is smaller than the bounds of the stream it belongs in. This is not supported.");
                // Flash Player just produces a black image in this case!
            }

            if width > bounds.0 as usize {
                // Removing the unwanted pixels on the right edge (most commonly: unused pieces of macroblocks)
                // by squishing all the rows tightly next to each other.
                // Bitmap at the moment does not allow these gaps, so we need to remove them.
                let new_width = bounds.0 as usize;
                let new_height = usize::min(height, bounds.1 as usize);
                // no need to move the first row, nor any rows on the bottom that will end up being cropped entirely
                for row in 1..new_height {
                    rgba.copy_within(
                        row * width * 4..(row * width + new_width) * 4,
                        row * new_width * 4,
                    );
                }
                width = new_width;
                height = new_height;
            }

            // Cropping the unwanted rows on the bottom, also dropping any unused space at the end left by the squish above
            height = usize::min(height, bounds.1 as usize);
            rgba.truncate(width * height * 4);

            Ok(DecodedFrame {
                width: width as u16,
                height: height as u16,
                rgba,
            })
        }
    }

    impl Default for Vp6Decoder {
        fn default() -> Self {
            Self::new(false, (0, 0))
        }
    }
}
