// The one address of a guide on the website. The server serves no docs, so
// every link from the web UI to a guide is built here and bound through
// outsideLink. A slug is a page under the website's docs; an anchor is a
// heading id there (GitHub style: lower case, spaces as hyphens).
const DOCS_BASE = 'https://droppedneedle.com/docs/';

export function docsUrl(slug: string, anchor?: string): string {
	return `${DOCS_BASE}${slug}${anchor ? `#${anchor}` : ''}`;
}
