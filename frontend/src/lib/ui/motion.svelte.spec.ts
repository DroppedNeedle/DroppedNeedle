import { cdp } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../app.css';
import { fade } from './motion';

const reduceMotion = (on: boolean) =>
	cdp().send('Emulation.setEmulatedMedia', {
		features: [{ name: 'prefers-reduced-motion', value: on ? 'reduce' : 'no-preference' }]
	});

function pulsing(extra = '') {
	const el = document.createElement('div');
	el.className = `animate-pulse ${extra}`;
	document.body.append(el);
	return el;
}

describe('reduced motion', () => {
	afterEach(async () => {
		document.body.replaceChildren();
		await reduceMotion(false);
	});

	it('stops CSS animation and Svelte transitions, unless an element keeps its motion', async () => {
		{
			// Stops CSS animation unless the element keeps its motion.
			const still = pulsing();
			const kept = pulsing('motion-keep');
			await reduceMotion(true);

			await expect
				.poll(() => Number.parseFloat(getComputedStyle(still).animationDuration))
				.toBeLessThan(0.001);
			expect(getComputedStyle(kept).animationDuration).toBe('2s');
		}

		{
			// Runs Svelte transitions at once.
			document.body.replaceChildren();
			await reduceMotion(false);
			const node = document.createElement('div');
			expect(fade(node, { duration: 400 }).duration).toBe(400);

			await reduceMotion(true);

			await expect.poll(() => fade(node, { duration: 400 }).duration).toBe(0);
		}
	});
});
