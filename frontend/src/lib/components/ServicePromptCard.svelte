<script lang="ts">
	import type { ServicePrompt } from '$lib/queries/discover/types';
	import { ArrowRight, Headphones, Tv, Download, Radio, Music, X } from 'lucide-svelte';
	import { dismiss } from '$lib/utils/dismissedPrompts';
	import type { ComponentType } from 'svelte';
	import Button, { type ButtonVariant } from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

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

	function getPromptButtonVariant(color: string): ButtonVariant {
		switch (color) {
			case 'primary':
				return 'primary';
			case 'secondary':
				return 'outline';
			case 'accent':
				return 'accent';
			default:
				return 'neutral';
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

<Card
	variant="bordered"
	padding="md"
	class="overflow-hidden border-l-4 {getBorderColor()}"
	style={getBorderStyle()}
>
	<IconButton
		size="xs"
		icon={X}
		label="Dismiss {prompt.title}"
		class="absolute top-2 right-2"
		onclick={handleDismiss}
	/>
	<div class="flex flex-col gap-4 sm:flex-row sm:items-center">
		<div class="shrink-0" style={getIconColor()}>
			<SvelteComponent class="h-10 w-10 sm:h-12 sm:w-12" />
		</div>
		<div class="min-w-0 flex-1 pr-6">
			<CardTitle as="h3" class="mb-1 text-base sm:text-lg">{prompt.title}</CardTitle>
			<p class="mb-2 text-xs text-base-content/70 sm:mb-3 sm:text-sm">
				{prompt.description}
			</p>
			<div class="flex flex-wrap gap-1 sm:gap-2">
				{#each prompt.features as feature, i (`${feature}-${i}`)}
					<span class="badge badge-ghost badge-xs sm:badge-sm">{feature}</span>
				{/each}
			</div>
		</div>
		<div class="shrink-0">
			<Button
				variant={getPromptButtonVariant(prompt.color)}
				size="sm"
				icon={ArrowRight}
				iconPosition="end"
				href={getSettingsLink(prompt.service)}
			>
				Connect
			</Button>
		</div>
	</div>
</Card>
