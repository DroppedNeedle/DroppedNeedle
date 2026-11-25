<!--
	The one piece of hardware on the counter: the "Play the week" key with
	its lamp, a dark readout saying what the sampler plays and from where,
	with a line that fills over the clip, and on a desktop the sampler's own
	volume as a knob. The readout is a polite live region.
-->
<script lang="ts">
	import { Pause, Play } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Knob from '$lib/ui/Knob.svelte';
	import Panel from '$lib/ui/Panel.svelte';
	import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';

	interface Props {
		/** "Play the week" is playing: the key stays down and lit. */
		weekPlaying?: boolean;
		/** Where the week has got to: "3 of 8". */
		weekPosition?: string | null;
		/** There is nothing in the week left to play. */
		weekOff?: boolean;
		onweek: () => void;
	}

	let { weekPlaying = false, weekPosition = null, weekOff = false, onweek }: Props = $props();

	const SOURCES = { deezer: 'Deezer', itunes: 'iTunes' } as const;

	const playing = $derived(deckSampler.status === 'playing' || deckSampler.status === 'paused');
	const loading = $derived(deckSampler.status === 'loading');
	const source = $derived.by(() => {
		const from = deckSampler.currentTrack?.source ?? deckSampler.provider;
		return from === 'deezer' || from === 'itunes' ? SOURCES[from] : '';
	});
	const head = $derived.by(() => {
		if (weekPlaying)
			return weekPosition ? `Playing the week - ${weekPosition}` : 'Playing the week';
		if (playing || loading) return source ? `Sample - ${source}` : 'Sample';
		return 'Listening post';
	});
	const line = $derived(
		loading
			? 'Finding samples'
			: playing
				? (deckSampler.currentTrack?.title ?? deckSampler.currentEntry?.title ?? '')
				: 'Nothing playing'
	);
</script>

<Panel look="metal" padding="none" class="dn-post" role="group" aria-label="Listening post">
	<Button
		variant="key"
		size="display-sm"
		class="dn-post__key"
		icon={weekPlaying ? Pause : Play}
		lamp={weekPlaying || playing || loading}
		active={weekPlaying}
		disabled={weekOff && !weekPlaying}
		onclick={onweek}
	>
		{weekPlaying ? 'Stop' : 'Play the week'}
	</Button>
	<div class="dn-post__readout" aria-live="polite">
		<span>{head}</span>
		<b>{line}</b>
		{#if playing}
			<span class="dn-post__bar motion-keep" aria-hidden="true">
				<i style:scale="{deckSampler.progress} 1"></i>
			</span>
		{/if}
	</div>
	<div class="dn-post__vol">
		<Knob
			label="Sample volume"
			size="xs"
			caption="none"
			lit="action"
			min={0}
			max={100}
			step={5}
			value={Math.round(deckSampler.volume * 100)}
			format={(v) => `${v}%`}
			onchange={(v) => deckSampler.setVolume(v / 100)}
		/>
	</div>
</Panel>

<style>
	:global(.dn-post) {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
		padding: 0 0.75rem 0 0.75rem;
	}
	:global(.dn-post__key) {
		flex: none;
	}
	/* glass stays dark in both themes */
	.dn-post__readout {
		display: flex;
		flex: 1;
		flex-direction: column;
		justify-content: center;
		gap: 3px;
		min-width: 0;
		height: 2.75rem;
		padding: 0.375rem 0.625rem;
		border-radius: 6px;
		background: var(--dn-vinyl);
		box-shadow: inset 0 1px 5px rgb(0 0 0 / 0.9);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		line-height: 1.15;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
		color: rgb(255 255 255 / 0.62);
		overflow: hidden;
	}
	.dn-post__readout > span:first-child {
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.dn-post__readout b {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		color: var(--dn-action);
	}
	.dn-post__bar {
		height: 2px;
		overflow: hidden;
		border-radius: 1px;
		background: rgb(255 255 255 / 0.12);
	}
	.dn-post__bar i {
		display: block;
		height: 100%;
		background: var(--dn-action);
		transform-origin: left;
		transition: scale 100ms linear;
	}
	.dn-post__vol {
		flex: none;
	}
	@media (max-width: 639px) {
		:global(.dn-post) {
			padding: 0.625rem 0.75rem;
		}
		.dn-post__vol {
			display: none;
		}
	}
</style>
