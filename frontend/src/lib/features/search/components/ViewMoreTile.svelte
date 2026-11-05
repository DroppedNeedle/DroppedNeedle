<!--
	The last tile of a search shelf, opening the full list: a round outline
	after artists, a dashed square after albums, the size of the tiles beside
	it.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';

	interface Props {
		kind: 'artist' | 'album';
		href: string;
		/** Names the link: "View more artists". */
		what: string;
	}

	let { kind, href, what }: Props = $props();
</script>

<div class="search-more" data-kind={kind}>
	<a {href} class="search-more__box">
		<span class="search-more__words">View<br />more</span>
		<ArrowRight class="size-5" strokeWidth={2.5} aria-hidden="true" />
		<span class="sr-only">{what}</span>
	</a>
</div>

<style>
	.search-more {
		min-width: 0;
	}

	.search-more__box {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		aspect-ratio: 1;
		border: 2px dashed color-mix(in oklab, var(--dn-title) 70%, transparent);
		border-radius: 0.5rem;
		color: var(--dn-title);
		text-align: center;
		transition:
			background-color 150ms,
			border-color 150ms;
	}
	.search-more__box:hover {
		border-color: var(--dn-title);
		background-color: color-mix(in oklab, var(--dn-title) 10%, transparent);
	}
	.search-more__box:focus-visible {
		outline: 3px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.search-more[data-kind='artist'] .search-more__box {
		width: 82%;
		margin: 0.5rem auto 0;
		border-style: solid;
		border-radius: 9999px;
	}

	.search-more__words {
		font-family: var(--font-display);
		font-size: 1.625rem;
		line-height: 0.95;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		font-synthesis: none;
	}
	@media (max-width: 639px) {
		.search-more__words {
			font-size: 1.375rem;
		}
	}
</style>
