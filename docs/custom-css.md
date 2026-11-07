# Custom CSS

How to restyle DroppedNeedle with your own CSS, and which parts of the page you can rely on.

## Style hooks

Key regions of the app carry a `data-part` attribute. These names are kept across releases: renaming or removing one is announced in the release notes. Anything else, such as class names and the structure of the markup, may change in any release, so a rule that targets it can stop working after an upgrade.

Target the attribute with an attribute selector:

```css
[data-part="media-tile-title"] {
  font-size: 0.8rem;
}

[data-part="sidebar"] [data-part="button"] {
  border-radius: 0;
}
```

| Name                  | What it is                                                      |
| --------------------- | --------------------------------------------------------------- |
| `shell`               | The signed-in app: sidebar and page column together.            |
| `sidebar`             | The navigation rail on the left, from tablet width up.          |
| `top-bar`             | The bar along the top with search, status and the theme switch. |
| `bottom-bar`          | The navigation bar along the bottom on a phone.                 |
| `player-bar`          | The player along the bottom while something plays.              |
| `player-bar-now`      | The player's now-playing art, title and artist.                 |
| `player-bar-controls` | The player's playback controls.                                 |
| `page`                | The page content area beside the sidebar and under the top bar. |
| `page-header`         | A browse page's title block.                                    |
| `page-title`          | The title inside a page's header.                               |
| `section-head`        | A shelf's heading row, with its See all link.                   |
| `section-head-title`  | The title inside a shelf's heading row.                         |
| `card`                | A panel that groups content.                                    |
| `media-tile`          | An album, artist or playlist tile in a grid or shelf.           |
| `media-tile-art`      | A tile's cover or photo.                                        |
| `media-tile-title`    | A tile's name under its art.                                    |
| `modal`               | A dialog's box.                                                 |
| `button`              | A button, or a link drawn as one.                               |
