# Tiny Desk downloader

* This will use yt-dlp to download a tiny desk concert from a URL
* The URL is scraped to get the list of songs in the concert and this is output as a plain text file

The set list usually looks like this in the HTML:

```html
<div id="storytext" class="storytext storylocation linkLocation">
...
<p><strong>SET LIST</strong><br></p>
<ul class="edTag"><li>"Wants Needs"</li><li>"Sober"</li><li>"Too Much"</li><li>"17"</li></ul>
...
</div>
```