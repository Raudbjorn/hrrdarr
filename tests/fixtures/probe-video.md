`probe-video.webm` is a synthetic one-frame, 16x16 black VP9 video with no audio.
It contains no user media. Generated locally with ffmpeg:

```sh
ffmpeg -nostdin -v error -f lavfi -i color=c=black:s=16x16:r=1 -frames:v 1 -c:v libvpx-vp9 -threads 1 -an probe-video.webm
```

The probe regression checks measured duration, dimensions and zero audio streams;
it does not require byte-identical encoder output. WAV audio is generated in the
test itself. Real probing runs when ffprobe is installed; its absence exercises
the explicit unavailable result and prints which measurement was not exercised.
