Split a tiny desk concert into individual songs.

```sh
./extract.sh <CONCERT_URL>
```

## Dependencies

* yt-dlp (downloading)
* ffmpeg (splitting)
* Rust (compiling this project)

### Running

Download a concert:

```sh
./download.sh <CONCERT_UR>
```

Split the concert:

```sh
cargo run --bin live-set-splitter -- <json_file>
```


### Overview

* [./concert-downloader](./concert-downloader) automation to help download the concert and scrape metadata.
* [./live-set-song-splitter](./live-set-song-splitter) split a tiny desk mp4 into individual tracks with metadata.
