import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Scope from './Scope.svelte';

/** The heights a trace's path visits, in the picture's own units. */
function heights(path: SVGPathElement): number[] {
	const d = path.getAttribute('d') ?? getComputedStyle(path).getPropertyValue('d');
	return [...d.matchAll(/[ML]\s*[\d.]+[ ,]([-\d.]+)/g)].map((m) => Number(m[1]));
}

describe('Scope', () => {
	it('is one picture named by its sentence, drawing both traces from their numbers', async () => {
		await render(Scope, {
			label: 'How Supernatural and The Ash sound, over each other',
			seed: [-2, 0.5, 3, -1],
			tuned: [-1.5, 1, 2.5, -0.5]
		});
		const scope = page.getByRole('img', {
			name: 'How Supernatural and The Ash sound, over each other'
		});
		await expect.element(scope).toBeVisible();
		const el = scope.element();
		const seed = el.querySelector<SVGPathElement>('[data-trace="seed"]');
		const tuned = el.querySelector<SVGPathElement>('[data-trace="tuned"]');
		expect(seed).not.toBeNull();
		expect(tuned).not.toBeNull();
		expect(heights(seed!)).toHaveLength(4);
		// The white trace is a plain attribute too, for browsers without CSS `d`.
		expect(tuned!.getAttribute('d')).toMatch(/^M/);
	});

	it('scales the two traces together, negative values included, inside the glass', async () => {
		await render(Scope, { label: 'Two traces', seed: [-4, 0, 2], tuned: [-1, 1, 4] });
		const el = page.getByRole('img', { name: 'Two traces' }).element();
		const seed = heights(el.querySelector<SVGPathElement>('[data-trace="seed"]')!);
		const tuned = heights(el.querySelector<SVGPathElement>('[data-trace="tuned"]')!);
		// The lowest of both sits lowest and the highest of both highest: one scale.
		expect(Math.max(...seed, ...tuned)).toBe(seed[0]);
		expect(Math.min(...seed, ...tuned)).toBe(tuned[2]);
		for (const at of [...seed, ...tuned]) {
			expect(at).toBeGreaterThan(0);
			expect(at).toBeLessThan(300);
		}
	});
});
