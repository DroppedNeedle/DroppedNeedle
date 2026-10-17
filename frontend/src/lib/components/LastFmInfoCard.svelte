<script lang="ts">
	import { ChevronUp, ChevronDown, ExternalLink, Users, Headphones } from 'lucide-svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { outsideLink } from '$lib/components/outsideLink';
	import { formatListenCount } from '$lib/utils/formatting';
	import { onMount } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		text: string | null | undefined;
		tags: { name: string; url?: string | null }[];
		listeners: number;
		playcount: number;
		url: string | null | undefined;
		loading?: boolean;
		enabled?: boolean;
	}

	let { text, tags, listeners, playcount, url, loading = false, enabled = true }: Props = $props();
	const link = $derived(outsideLink(url));

	let textExpanded = $state(false);
	let textElement: HTMLElement | undefined = $state();
	let showReadMore = $state(false);

	function checkTextHeight() {
		if (textElement && !textExpanded) {
			const lineHeight = parseFloat(getComputedStyle(textElement).lineHeight);
			const actualHeight = textElement.scrollHeight;
			showReadMore = actualHeight > lineHeight * 4;
		}
	}

	onMount(() => {
		setTimeout(() => checkTextHeight(), 50);
	});

	$effect(() => {
		if (text) {
			setTimeout(() => checkTextHeight(), 50);
		}
	});

	let hasText = $derived(!!text);
	let hasTags = $derived(tags.length > 0);
	let hasStats = $derived(listeners > 0 || playcount > 0);
	let hasContent = $derived(hasText || hasTags || hasStats);
</script>

{#if loading}
	<Card>
		<div class="flex items-center gap-2 mb-3">
			<div class="skeleton h-5 w-5 rounded-full"></div>
			<div class="skeleton h-5 w-24"></div>
		</div>
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-3/4"></div>
		<div class="flex gap-2 mt-3">
			<div class="skeleton h-6 w-16 rounded-full"></div>
			<div class="skeleton h-6 w-20 rounded-full"></div>
			<div class="skeleton h-6 w-14 rounded-full"></div>
		</div>
	</Card>
{:else if !enabled}{:else if hasContent}
	<Card>
		<div class="flex items-center justify-between">
			<div class="flex items-center gap-2">
				<span class="badge badge-sm border-transparent bg-lastfm/15 text-lastfm">Last.fm</span>
			</div>
			{#if link}
				<Button
					variant="ghost"
					size="xs"
					icon={ExternalLink}
					href={link}
					target="_blank"
					rel="noopener noreferrer"
				>
					View on Last.fm
				</Button>
			{/if}
		</div>

		{#if hasStats}
			<div class="flex flex-wrap gap-4 text-sm">
				{#if listeners > 0}
					<span class="flex items-center gap-1.5 text-base-content/70">
						<Users class="h-4 w-4" />
						{formatListenCount(listeners, true)} listeners
					</span>
				{/if}
				{#if playcount > 0}
					<span class="flex items-center gap-1.5 text-base-content/70">
						<Headphones class="h-4 w-4" />
						{formatListenCount(playcount)}
					</span>
				{/if}
			</div>
		{/if}

		{#if hasText}
			<div class="text-sm sm:text-base text-base-content/80 leading-relaxed">
				{#if textExpanded}
					<div>{text}</div>
					<Button
						variant="ghost"
						size="sm"
						icon={ChevronUp}
						iconPosition="end"
						class="mt-2"
						onclick={() => (textExpanded = false)}
					>
						Show Less
					</Button>
				{:else}
					<div bind:this={textElement} class="line-clamp-4 overflow-hidden">
						{text}
					</div>
					{#if showReadMore}
						<Button
							variant="ghost"
							size="sm"
							icon={ChevronDown}
							iconPosition="end"
							class="mt-2"
							onclick={() => (textExpanded = true)}
						>
							Read More
						</Button>
					{/if}
				{/if}
			</div>
		{/if}

		{#if hasTags}
			<div class="flex flex-wrap gap-2">
				{#each tags.slice(0, 10) as tag (tag.name)}
					<a
						href={withBasePath(`/genre?name=${encodeURIComponent(tag.name)}`)}
						class="badge badge-outline badge-sm cursor-pointer hover:badge-primary transition-colors"
					>
						{tag.name}
					</a>
				{/each}
			</div>
		{/if}
	</Card>
{/if}
