<script lang="ts">
	import type { ServicePrompt } from '$lib/types';
	import { ArrowRight, Headphones, Tv, Download, Radio, Music, X } from 'lucide-svelte';
	import { dismiss } from '$lib/utils/dismissedPrompts';
	import type { ComponentType } from 'svelte';

	const serviceIcons: Record<string, ComponentType> = {
		listenbrainz: Headphones,
		jellyfin: Tv,
		'download-client': Download,
		lastfm: Radio
	};

	const serviceBrandVars: Record<string, string> = {
		listenbrainz: '--brand-listenbrainz',
		jellyfin: '--brand-jellyfin',
		'download-client': 'accent',
		lastfm: '--brand-lastfm'
	};

	interface Props {
		prompt: ServicePrompt;
		ondismiss?: ((service: string) => void) | undefined;
	}

	let { prompt, ondismiss = undefined }: Props = $props();

	function getBorderColor(): string {
		const v = serviceBrandVars[prompt.service];
		if (!v) return 'border-l-base-content/30';
		if (v === 'accent') return 'border-l-accent';
		return '';
	}

	function getBorderStyle(): string {
		const v = serviceBrandVars[prompt.service];
		if (!v || v === 'accent') return '';
		return `border-left-color: rgb(var(${v}));`;
	}

	function getIconColor(): string {
		const v = serviceBrandVars[prompt.service];
		if (!v) return '';
		if (v === 'accent') return 'color: var(--color-accent);';
		return `color: rgb(var(${v}));`;
	}

	function getPromptButtonClass(color: string): string {
		switch (color) {
			case 'primary':
				return 'btn-primary';
			case 'secondary':
				return 'btn-secondary';
			case 'accent':
				return 'btn-accent';
			default:
				return 'btn-neutral';
		}
	}

	function getSettingsLink(service: string): string {
		// Per-user scrobble/discovery connections live on /profile, not the admin /settings tabs.
		if (service === 'listenbrainz' || service === 'lastfm') return '/profile#scrobbling';
		return `/settings?tab=${service}`;
	}

	function handleDismiss() {
		dismiss(prompt.service);
		ondismiss?.(prompt.service);
	}

	const SvelteComponent = $derived(serviceIcons[prompt.service] || Music);
</script>

<article
	class="relative overflow-hidden rounded-2xl border border-base-content/8 border-l-2 bg-base-200/50 transition-colors hover:bg-base-200 {getBorderColor()}"
	style={getBorderStyle()}
>
	<button
		class="btn btn-ghost btn-xs btn-circle absolute top-2.5 right-2.5 text-base-content/40 hover:text-base-content"
		onclick={handleDismiss}
		aria-label="Dismiss {prompt.title}"
		title="Dismiss"
	>
		<X class="h-3.5 w-3.5" />
	</button>

	<div class="flex flex-col gap-4 p-5 sm:flex-row sm:items-center sm:gap-5">
		<div
			class="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl bg-base-content/6"
			style={getIconColor()}
		>
			<SvelteComponent class="h-5 w-5" />
		</div>

		<div class="min-w-0 flex-1 pr-8 sm:pr-0">
			<p
				class="mb-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em]"
				style={getIconColor()}
			>
				{prompt.service}
			</p>
			<h3 class="font-display text-base font-semibold tracking-tight sm:text-lg">
				{prompt.title}
			</h3>
			<p class="mt-1 text-xs text-base-content/60 sm:text-sm">
				{prompt.description}
			</p>
			{#if prompt.features.length > 0}
				<div class="mt-2.5 flex flex-wrap gap-1.5">
					{#each prompt.features as feature, i (`${feature}-${i}`)}
						<span
							class="rounded-full border border-base-content/10 px-2.5 py-0.5 text-[0.68rem] text-base-content/55"
						>
							{feature}
						</span>
					{/each}
				</div>
			{/if}
		</div>

		<div class="shrink-0">
			<a
				href={getSettingsLink(prompt.service)}
				class="btn btn-sm gap-1.5 {getPromptButtonClass(prompt.color)}"
			>
				Connect
				<ArrowRight class="h-4 w-4" />
			</a>
		</div>
	</div>
</article>
