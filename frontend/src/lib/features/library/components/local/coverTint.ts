// The deck's glow takes one colour from the playing cover: the average of
// its brighter, more coloured pixels, so a mostly black sleeve still gives
// its accent. A cover the browser cannot read (another host refusing CORS)
// gives null and the deck keeps its own glow.
const cache = new Map<string, string | null>();

export function coverTint(url: string): Promise<string | null> {
	const known = cache.get(url);
	if (known !== undefined) return Promise.resolve(known);
	return new Promise((resolve) => {
		const img = new Image();
		img.crossOrigin = 'anonymous';
		img.decoding = 'async';
		const done = (tint: string | null) => {
			cache.set(url, tint);
			resolve(tint);
		};
		img.onerror = () => done(null);
		img.onload = () => {
			try {
				const size = 32;
				const canvas = document.createElement('canvas');
				canvas.width = size;
				canvas.height = size;
				const ctx = canvas.getContext('2d', { willReadFrequently: true });
				if (!ctx) return done(null);
				ctx.drawImage(img, 0, 0, size, size);
				const px = ctx.getImageData(0, 0, size, size).data;
				let r = 0;
				let g = 0;
				let b = 0;
				let weight = 0;
				for (let i = 0; i < px.length; i += 4) {
					const max = Math.max(px[i], px[i + 1], px[i + 2]);
					const min = Math.min(px[i], px[i + 1], px[i + 2]);
					const w = 0.15 + (max - min) / 255 + max / 510;
					r += px[i] * w;
					g += px[i + 1] * w;
					b += px[i + 2] * w;
					weight += w;
				}
				if (!weight) return done(null);
				done(`rgb(${Math.round(r / weight)} ${Math.round(g / weight)} ${Math.round(b / weight)})`);
			} catch {
				done(null);
			}
		};
		img.src = url;
	});
}
