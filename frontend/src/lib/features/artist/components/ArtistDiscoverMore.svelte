<!--
	What the artist page shows in place of its popular lists and similar
	artists while no listening service is linked: one card that says what a
	service would add, and the way to connect one.
-->
<script lang="ts">
	import { Plug } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		artistName: string;
		/** Layout only. */
		class?: string;
	}

	let { artistName, class: className = '' }: Props = $props();
</script>

<Card
	as="section"
	variant="bordered"
	padding="none"
	class="dn-more overflow-hidden {className}"
	aria-labelledby="artist-discover-more-title"
>
	<div class="dn-more__body">
		<div class="dn-more__deck" aria-hidden="true">
			<VinylRecord sheen="acid" class="dn-more__record" />
		</div>
		<div class="dn-more__text">
			<h2 id="artist-discover-more-title" class="dn-more__title">Discover more</h2>
			<p class="mt-1 text-sm">
				Connect a music service to see popular albums, popular songs and similar artists for {artistName}.
			</p>
		</div>
		<Button
			variant="primary"
			icon={Plug}
			class="dn-more__cta"
			href={withBasePath('/profile#scrobbling')}>Connect a service</Button
		>
	</div>
</Card>

<style>
	.dn-more__body {
		position: relative;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 1rem 1.5rem;
		padding: 1.25rem 1.25rem 1.25rem 8.5rem;
		min-height: 7.5rem;
	}

	.dn-more__deck {
		position: absolute;
		inset: 0 auto 0 0;
		width: 7.5rem;
		overflow: hidden;
	}
	.dn-more__deck :global(.dn-more__record) {
		position: absolute;
		top: 50%;
		right: 0;
		width: 13rem;
		translate: 0 -50%;
	}

	.dn-more__text {
		flex: 1 1 16rem;
		min-width: 0;
	}

	.dn-more__title {
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1;
		color: var(--color-base-content);
	}

	.dn-more__body :global(.dn-more__cta) {
		margin-left: auto;
	}

	@media (prefers-contrast: more), (forced-colors: active) {
		.dn-more__body {
			padding-left: 1.25rem;
		}
	}
</style>
