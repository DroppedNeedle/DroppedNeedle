<script lang="ts" module>
	import DOMPurify from 'dompurify';
	import { marked } from 'marked';

	import { PROSE_LINK_URL, WEB_URL } from './outsideLink';

	// The only place a string becomes markup. Everything not written in this
	// codebase (provider notes, release notes, plugin text) is rendered here.
	// The allowlist is explicit: text formatting, lists, tables and links,
	// plus images in DroppedNeedle's own release notes. No scripts, frames,
	// forms, SVG, MathML, styles, classes, ids or event handlers.
	const TEXT_TAGS = [
		...['p', 'br', 'hr', 'div', 'span', 'blockquote', 'pre', 'code', 'kbd'],
		...['h1', 'h2', 'h3', 'h4', 'h5', 'h6'],
		...['b', 'strong', 'i', 'em', 'u', 's', 'del', 'ins', 'sub', 'sup'],
		...['ul', 'ol', 'li', 'dl', 'dt', 'dd'],
		...['table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'caption'],
		...['details', 'summary', 'a']
	];
	// DOMPurify holds every attribute value to the URL pattern unless the
	// attribute is declared as not carrying a URL.
	const PLAIN_ATTR = ['alt', 'title', 'width', 'height', 'align', 'start', 'colspan', 'rowspan'];

	let purifier: ReturnType<typeof DOMPurify> | undefined;

	function getPurifier() {
		if (purifier) return purifier;
		// A private instance, so the hook below cannot leak into other uses.
		purifier = DOMPurify(window);
		purifier.addHook('afterSanitizeAttributes', (node) => {
			const href = node.getAttribute('href');
			if (href !== null && !(node.tagName === 'A' && PROSE_LINK_URL.test(href))) {
				node.removeAttribute('href');
			}
			// DOMPurify lets data: URIs through on images whatever the URL
			// pattern says, so the scheme is checked again here.
			const src = node.getAttribute('src');
			if (src !== null && !(node.tagName === 'IMG' && WEB_URL.test(src))) {
				node.removeAttribute('src');
			}
			if (node.tagName === 'A' && node.hasAttribute('href')) {
				node.setAttribute('target', '_blank');
				node.setAttribute('rel', 'noopener noreferrer');
			}
			if (node.tagName === 'IMG') {
				node.setAttribute('loading', 'lazy');
				node.setAttribute('referrerpolicy', 'no-referrer');
			}
		});
		return purifier;
	}

	// Third-party notes get no images: one would make every reader's browser
	// fetch an address of the author's choosing.
	function sanitize(html: string, images: boolean): string {
		return getPurifier().sanitize(html, {
			ALLOWED_TAGS: images ? [...TEXT_TAGS, 'img'] : TEXT_TAGS,
			ALLOWED_ATTR: [...(images ? ['href', 'src'] : ['href']), ...PLAIN_ATTR],
			ADD_URI_SAFE_ATTR: PLAIN_ATTR,
			ALLOWED_URI_REGEXP: PROSE_LINK_URL,
			ALLOWED_NAMESPACES: ['http://www.w3.org/1999/xhtml'],
			ALLOW_DATA_ATTR: false,
			ALLOW_ARIA_ATTR: false,
			ALLOW_UNKNOWN_PROTOCOLS: false,
			SANITIZE_DOM: true,
			KEEP_CONTENT: true
		});
	}
</script>

<script lang="ts">
	type Props = { class?: string } & (
		{ html: string; markdown?: never } | { markdown: string; html?: never }
	);

	let { class: className = '', html, markdown }: Props = $props();

	// Markdown may carry inline HTML, so its output is sanitized like any other.
	// This sits in the root layout through the release notes modal, so text
	// that fails to parse renders as nothing instead of breaking the page.
	const safe = $derived.by(() => {
		try {
			return markdown !== undefined
				? sanitize(marked.parse(markdown, { async: false }), true)
				: sanitize(html ?? '', false);
		} catch {
			return '';
		}
	});
</script>

<div class={className}>{@html safe}</div>
