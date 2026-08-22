<script lang="ts">
	import { resolve } from '$app/paths';
	import { ChevronUp, ChevronDown, ExternalLink, Users, Headphones } from 'lucide-svelte';
	import { formatListenCount } from '$lib/utils/formatting';
	import { onMount } from 'svelte';

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
	<div class="bg-base-200/50 rounded-box p-4 sm:p-6 space-y-3">
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
	</div>
{:else if !enabled}{:else if hasContent}
	<div class="bg-base-200/50 rounded-box p-4 sm:p-6 space-y-4">
		<div class="flex items-center justify-between">
			<div class="flex items-center gap-2">
				<span
					class="badge badge-sm"
					style="background-color: rgb(var(--brand-lastfm) / 0.15); color: rgb(var(--brand-lastfm));"
				>
					Last.fm
				</span>
			</div>
			{#if url}
				<a
					href={url}
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-ghost btn-xs gap-1 opacity-60 hover:opacity-100"
				>
					<ExternalLink class="h-3 w-3" />
					View on Last.fm
				</a>
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
					<button
						class="btn btn-sm btn-ghost mt-2 gap-1 text-xs"
						onclick={() => (textExpanded = false)}
					>
						Show Less
						<ChevronUp class="h-3 w-3" />
					</button>
				{:else}
					<div
						bind:this={textElement}
						class="line-clamp-4 overflow-hidden"
						style="display: -webkit-box; -webkit-box-orient: vertical;"
					>
						{text}
					</div>
					{#if showReadMore}
						<button
							class="btn btn-sm btn-ghost mt-2 gap-1 text-xs"
							onclick={() => (textExpanded = true)}
						>
							Read More
							<ChevronDown class="h-3 w-3" />
						</button>
					{/if}
				{/if}
			</div>
		{/if}

		{#if hasTags}
			<div class="flex flex-wrap gap-2">
				{#each tags.slice(0, 10) as tag (tag.name)}
					<a
						href={resolve(`/genre?name=${encodeURIComponent(tag.name)}`)}
						class="badge badge-outline badge-sm cursor-pointer hover:badge-primary transition-colors"
					>
						{tag.name}
					</a>
				{/each}
			</div>
		{/if}
	</div>
{/if}
