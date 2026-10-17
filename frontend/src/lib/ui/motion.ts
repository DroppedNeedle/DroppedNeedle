// Svelte transitions run in JS, so the reduced-motion rule in app.css never
// reaches them. These wrap Svelte's own and finish at once when the viewer
// asks for less motion. Import transitions from here, not from svelte/transition.
import { flip as svelteFlip, type AnimationConfig, type FlipParams } from 'svelte/animate';
import { prefersReducedMotion } from 'svelte/motion';
import {
	fade as svelteFade,
	fly as svelteFly,
	scale as svelteScale,
	slide as svelteSlide,
	type FadeParams,
	type FlyParams,
	type ScaleParams,
	type SlideParams,
	type TransitionConfig
} from 'svelte/transition';

function still<P extends { duration?: unknown }>(params: P | undefined): P | undefined {
	return prefersReducedMotion.current ? ({ ...params, duration: 0 } as P) : params;
}

export function fade(node: Element, params?: FadeParams): TransitionConfig {
	return svelteFade(node, still(params));
}

export function fly(node: Element, params?: FlyParams): TransitionConfig {
	return svelteFly(node, still(params));
}

export function slide(node: Element, params?: SlideParams): TransitionConfig {
	return svelteSlide(node, still(params));
}

export function scale(node: Element, params?: ScaleParams): TransitionConfig {
	return svelteScale(node, still(params));
}

export function flip(
	node: Element,
	fromTo: { from: DOMRect; to: DOMRect },
	params?: FlipParams
): AnimationConfig {
	return svelteFlip(node, fromTo, still(params));
}
