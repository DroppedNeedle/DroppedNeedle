# Custom CSS

How to restyle DroppedNeedle with your own CSS, and which parts of the page you can rely on.

## What it is

Profile > Appearance > Custom CSS takes CSS rules of your own and applies them on top of the app's styles, on every signed-in page (not the sign-in, setup or app connect pages). It changes how the app looks for you alone: other people on the same server, and other accounts on the same browser, never see it.

Your CSS is kept on your account, with your theme, so it follows you to every browser and device you sign in on. Each browser also keeps a copy, so a reload paints your look at once without a flash. Signing out takes it off until you sign in again. A sheet can hold up to 20,000 characters.

If you used custom CSS or picked a theme before they moved to the account, the first sign-in on that browser moves them up by itself.

## Site CSS

An admin can add CSS for the whole server under Settings > Appearance (up to 50,000 characters). It loads on every signed-in page on top of the app's own styles, and your CSS loads on top of it, so your rules win where the two disagree.

The same section has "Let people add their own CSS", on by default. While it is off, nobody's own CSS is applied or can be saved. Your text is kept, the editor shows it read-only and says custom CSS is turned off on this server, and you can still clear it. An admin can also turn one person's CSS off from their user page: the text stays and "Use my CSS" goes off, so you can see what happened and switch it back on.

## The editor

The editor knows CSS: it colours the syntax, numbers the lines, indents with Tab and suggests names as you type, both the theme's `--dn-*` variables and the style hooks below. Tab indents inside the editor; to move on with the keyboard, press Escape and then Tab.

Each part the check would refuse (see below) is underlined on its own line, with the reason on hover and under the editor, and Save stays off until it is gone. The count under the editor shows how many characters you have used.

- **Use my CSS** turns your sheet off and on without deleting it.
- **Live preview**, on by default, applies what you type to the page after a short pause, as long as the check lets it through, so you see a change before you save it. The bar under the editor says when you are looking at unsaved changes. Cancel, or leaving the page, puts your saved sheet back.
- **Save** stores the sheet. The message that confirms it has an Undo button, which puts back the sheet you had before.
- If you leave the page with unsaved changes, the app asks first.

The rail beside the editor (under it on a phone) has three tabs:

- **Variables** lists the theme's variables with their values in the theme you are using now, and a swatch for each colour. Type in the filter to narrow the list, and click one to insert `var(--name)` at the cursor.
- **Snippets** inserts a starter rule at the cursor: square corners, compact lists, larger text and no decorations. Each one only uses the style hooks and variables, so it keeps working across releases.
- **History** lists your last 10 saves, newest first, with when each was saved and its length. Restore loads one into the editor without saving it, so you can look at it with live preview and then save or cancel.

## Import and download

Import loads a `.css` file from your device into the editor. It replaces what is there but is not saved until you press Save. A file longer than 20,000 characters is refused.

Download saves what is in the editor as `droppedneedle.css`.

## If something goes wrong

A sheet can hide or cover parts of the page. Any of these gets you out:

- Add `?nocustomcss=1` to the address and load it, for example `https://music.example.com/profile?nocustomcss=1`. Your CSS and the site CSS stay off until the next full reload, so you can fix your sheet, turn it off or clear it.
- Turn off "Use my CSS".
- Press Undo on the message that confirmed the save, or Restore an earlier save from History.
- Ask an admin: they can turn your CSS off from your user page, without seeing it.

## What the check refuses, and why

A sheet runs on every page you open, so nothing in it may reach outside the app: a sheet that could load an image or a font from another address would tell that address when and where you use DroppedNeedle. The server checks every sheet before it saves it, and the browser runs the same check before it applies one and while you type. A sheet is refused, with what was not allowed, when it holds:

- `url()`, `image-set()` or any other CSS function outside the set that only computes values: colours (`rgb()`, `oklch()`, `color-mix()` and the like), gradients, maths (`calc()`, `var()`, `min()`, `clamp()` and the like), transforms, filters, shapes, timing functions, grid functions and selector functions such as `:is()` and `:has()`;
- a backslash, because an escape can spell any name (`u\72l(` is `url(`);
- `@import` or `@font-face`.

The check also reads strings and comments, so a `url(` inside a comment is refused too, and a few harmless functions such as `sqrt()` or `@supports selector()` are not on the list.

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

## Variables

The app's own colours are CSS variables set for each theme, so one rule can recolour every place that uses them. Set them on `:root, [data-theme]` so the rule wins in every theme, or on `[data-theme="light"]` and the like for one theme only. The Variables tab shows each one's value in the theme you are using.

| Variable                  | What it colours                                         |
| ------------------------- | ------------------------------------------------------- |
| `--dn-muted`              | Secondary text.                                         |
| `--dn-title`              | Display headings.                                       |
| `--dn-action`             | The fill of the one main action on a screen.            |
| `--dn-action-content`     | The text on that fill.                                  |
| `--dn-action-edge`        | The main action's outline.                              |
| `--dn-paper`              | The light paper islands, such as an album's track list. |
| `--dn-paper-content`      | The text on paper.                                      |
| `--dn-paper-tile`         | A dark tile on paper.                                   |
| `--dn-paper-tile-content` | The text on a dark tile on paper.                       |
| `--dn-paper-border`       | Edges on paper.                                         |
| `--dn-focus`              | The keyboard focus ring.                                |
| `--dn-edge`               | Input and switch edges.                                 |
| `--dn-line`               | Plain dividers.                                         |
| `--dn-selected`           | The background of a selected row or choice.             |
| `--dn-frit`               | The dot frame and halftone prints.                      |
| `--dn-frit-opacity`       | How strongly those dots show; 0 hides them.             |
| `--dn-vinyl`              | The record behind a sleeve.                             |
| `--dn-vinyl-groove`       | The grooves on that record.                             |
| `--dn-arm`                | The turntable's tonearm.                                |

The daisyUI colours (`--color-base-100` for the page, `--color-base-200` and `--color-base-300` for surfaces, `--color-base-content` for text, `--color-primary`, `--color-error` and so on) and the corner radii (`--radius-box`, `--radius-field`, `--radius-selector`) can be set the same way. They are part of the theme rather than a promise, so a release may rename one.

## Examples

Square corners everywhere, and a softer muted text:

```css
:root,
[data-theme] {
  --radius-box: 0;
  --radius-field: 0;
  --radius-selector: 0;
  --dn-muted: oklch(70% 0.02 140);
}
```

Larger album and artist names in grids:

```css
[data-part="media-tile-title"] {
  font-size: 0.9375rem;
  font-weight: 600;
}
```

A different accent in the light theme only:

```css
[data-theme="light"] {
  --dn-title: rgb(120 40 140);
  --color-primary: rgb(120 40 140);
}
```

No dot frame or halftone prints:

```css
:root,
[data-theme] {
  --dn-frit-opacity: 0;
}
```
