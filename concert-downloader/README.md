# Tiny Desk downloader

``` sh
download.sh <CONCERT_URL>
```

* This will use yt-dlp to download a tiny desk concert from a URL
* The URL is scraped to get the list of songs in the concert and this is output as a json file

The scraper of the JSON file can be ran on its own:

``` sh
cargo run --bin scraper <CONCERT_UR>
```

## Listing concerts

This will list out concerts and their urls, the url can be given to the scraper.
You can also just use the tiny desk website and copy those urls from your browser.

```sh
cargo run --bin archive_scraper <YEAR> <MONTH> [DAY]
```

## Scraping

The set list usually looks like this in the HTML, but there are some other informations:

```html
<div id="storytext" class="storytext storylocation linkLocation">
...
<p><strong>SET LIST</strong><br></p>
<ul class="edTag"><li>"Wants Needs"</li><li>"Sober"</li><li>"Too Much"</li><li>"17"</li></ul>
...
</div>
```


## Testing

If there is a scraping failure, that can be added as a test case with:

```sh
cargo run --bin save_scrape_failure <CONCERT_URL>
```