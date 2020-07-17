## rigal

A simple [sigal](https://github.com/saimn/sigal) clone written in Rust.

### Usage

rigal is a command line application and uses various sub commands. First, create
a new base configuration using `rigal new` and edit `rigal.toml` to your liking,
especially adapt the `input` and `output` paths. `output` will be created if it
does not exist. Then run `rigal build` to generate the static output.

### Template variables

The following structure is injected into the current [Tera
context](https://tera.netlify.app/docs):

```json
{
  "album": {
    "title": "Title",
    "images: [
      {
        "image": "image.jpg",
        "thumbnail": "thumbnail.jpg"
      }
    ],
    "albums": [
      "link"
    ]
  }
}
```
