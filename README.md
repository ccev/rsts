# rsts

A high-performance Rust static map renderer and tile proxy, following [SwiftTileserverCache](https://github.com/123FLO321/SwiftTileserverCache).

Uses [Martin](https://martin.maplibre.org/) as a tileserver and [MapLibre Native](https://maplibre.org/projects/native/) to render static maps.

## Endpoints

- `GET/POST /staticmap`: Render a single static map.
- `GET/POST /staticmap/{template}`: Render a static map using a Tera template.
- `POST /multistaticmap`: Render a grid of static maps.
- `GET/POST /multistaticmap/{template}`: Render a multistaticmap using a Tera template.
- `GET /tiles/{id}/{z}/{x}/{y}`: Mirror and cache external tile sources.

## Setup

1. `git clone https://github.com/ccev/rsts && cd rsts`
2. `cp docker-compose.example.yml docker-compose.yml && cp martin.example.yml martin.yml && cp rsts.example.toml rsts.toml`
3. `docker compose pull`
4. We're now setting up martin. [Refer to their docs for additional guidelines](https://maplibre.org/martin/introduction.html)
5. Get your desired mbtiles and put them into `data/mbtiles` and call the file `{source name}.mbtiles`. 
You may create a syslink if you already have a file, or download them from [openmaptiles](https://openmaptiles.com/downloads/planet/)
6. Get your desired styles.
   1. Go to https://maputnik.github.io/editor/
   2. Click `Open`, choose your desired style, then click `Export` and `Download`
   3. Put the downloaded json file in `data/styles`. Call it `{style name}.json`
   4. Now open the json, towards the top you will find `sources.openmaptiles.url`. 
   Change the url to `https://your-public-domain.com/{source name}` with `{source name}` being your name from step 5.
   5. Under `sprite` you will find an url to a sprite repository. You could keep it, or host them yourself. 
   Download the assets and put them in `data/sprites/{style name}`, then add this path to `martin.yml`. 
   Now replace the url with `https://your-public-domain.com/sprite/{style name}`
   6. Replace the `glyphs` url with `https://your-public-domain.com/font/{fontstack}/{range}`, then serach for `text-font` in the json.
   You will find one, or multiple font families. Search the web for download links to them, then paste the files into `data/fonts`. 
   You may organize them into subdirectories.
7. `docker compose up -d` should now start the server. You can check `http://url:3000` for Martin's admin dashboard.
8. Make sure you either disable this admin dashboard for production use (uncomment `web_ui` in `martin.yml`) or put it behind basic auth
9. [TBD] Now, put both Martin and rsts behind a reverse proxy. You may also setup Cloudflare Cache rules

## AI notice

This project is entirely vibe-coded. Beware accordingly.