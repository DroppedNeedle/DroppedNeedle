<!--
	The hub of a server nobody has connected: what connecting it brings, and
	for an admin the way to its settings and what they will need. Everyone
	else is told an admin can connect it.
-->
<script lang="ts">
	import { ArrowRight, CirclePlay, Disc3, Download, Info, Link } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Illustration from '$lib/ui/Illustration.svelte';
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		remote: RemoteSourceDescriptor;
		isAdmin: boolean;
	}

	let { remote, isAdmin }: Props = $props();

	const needs: Record<RemoteSourceDescriptor['source'], string> = {
		navidrome: "You'll need the server URL, a username and a password.",
		jellyfin: "You'll need the server URL and an API key.",
		plex: 'Sign in with your Plex account and pick the server.'
	};

	const gets = [
		{ icon: Disc3, label: 'Browse your library' },
		{ icon: CirclePlay, label: 'Play albums and tracks' },
		{ icon: Download, label: 'Import playlists' }
	];
</script>

<div class="ns">
	<div class="ns__art" aria-hidden="true">
		<span class="ns__sheet"></span>
		<Illustration name="turntable" class="ns__deck" />
	</div>

	<Card variant="torn" padding="none" as="section" class="ns__card" aria-labelledby="ns-title">
		<h2 id="ns-title" class="ns__title">Connect {remote.label}</h2>
		<p class="ns__line">
			Link your {remote.label} server to browse your music, play albums and tracks, and import playlists
			into DroppedNeedle.
		</p>
		{#if isAdmin}
			<Button
				variant="primary"
				size="display"
				icon={Link}
				href={withBasePath(`/settings?tab=${remote.source}`)}
				class="ns__go"
			>
				Set up {remote.label}
				<ArrowRight class="size-5" aria-hidden="true" />
			</Button>
		{:else}
			<p class="ns__admin">An admin can connect {remote.label} in Settings.</p>
		{/if}
		<hr class="ns__rule" />
		<p class="ns__label">What you'll get</p>
		<ul class="ns__gets">
			{#each gets as get (get.label)}
				<li><get.icon class="ns__get-icon" aria-hidden="true" />{get.label}</li>
			{/each}
		</ul>
	</Card>
</div>

{#if isAdmin}
	<div class="ns__help" role="note">
		<Info class="ns__help-icon" aria-hidden="true" />
		<div class="min-w-0">
			<p class="ns__help-title">Already running {remote.label}?</p>
			<p class="ns__help-line">{needs[remote.source]}</p>
		</div>
	</div>
{/if}

<style>
	/* the card is the sleeve; the record slides out of it to the left */
	.ns {
		position: relative;
		isolation: isolate;
		display: grid;
		grid-template-columns: 16rem minmax(0, 1fr);
		align-items: center;
		max-width: 78rem;
		margin-inline: auto;
		padding-block: 1rem;
	}

	.ns__art {
		position: relative;
		z-index: -1;
		align-self: stretch;
		margin-right: -3rem;
	}
	.ns__sheet {
		position: absolute;
		inset: 0 2rem -0.5rem 0;
		background: var(--dn-action);
		transform: rotate(-5deg);
		clip-path: polygon(3% 2%, 100% 0, 97% 100%, 0 97%);
	}
	.ns__art :global(.ns__deck) {
		position: absolute;
		right: 0;
		top: 53%;
		height: 92%;
		width: auto;
		translate: 0 -50%;
		filter: drop-shadow(-8px 12px 18px color-mix(in oklab, black 45%, transparent));
	}
	.ns :global(.ns__card) {
		box-shadow: 0 18px 40px -18px color-mix(in oklab, black 70%, transparent);
	}

	.ns :global(.ns__card > .card-body) {
		gap: 0;
		padding: 2.5rem 2.75rem 2.25rem;
	}
	.ns__title {
		font-family: var(--font-display);
		font-size: 4rem;
		font-weight: 400;
		line-height: 0.95;
		text-transform: uppercase;
		font-synthesis: none;
		overflow-wrap: anywhere;
	}
	.ns__line {
		max-width: 38rem;
		margin-top: 0.875rem;
		font-size: 1.125rem;
		line-height: 1.45;
	}
	.ns__admin {
		margin-top: 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
	}
	.ns :global(.ns__go) {
		align-self: flex-start;
		margin-top: 1.5rem;
		border: 2px solid var(--dn-paper-content);
	}
	.ns__rule {
		margin-block: 1.75rem 1.25rem;
		border: 0;
		border-top: 1px solid color-mix(in oklab, var(--dn-paper-content) 30%, transparent);
	}
	.ns__label {
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 700;
		letter-spacing: 0.24em;
		text-transform: uppercase;
	}
	.ns__gets {
		display: flex;
		flex-wrap: wrap;
		gap: 1rem 2rem;
		margin-top: 1rem;
	}
	.ns__gets li {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		max-width: 11rem;
		font-family: var(--font-display);
		font-size: 1.125rem;
		line-height: 1.05;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.ns__gets li + li {
		padding-left: 2rem;
		border-left: 1px solid color-mix(in oklab, var(--dn-paper-content) 30%, transparent);
	}
	.ns__gets :global(.ns__get-icon) {
		width: 2.25rem;
		height: 2.25rem;
		flex: none;
		stroke-width: 1.75;
	}

	.ns__help {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		width: 100%;
		max-width: 60rem;
		margin: 1.5rem auto 0;
		padding: 1rem 1.5rem;
		border: 1px solid var(--dn-edge);
		border-radius: var(--radius-box);
		background: var(--color-base-200);
	}
	.ns__help :global(.ns__help-icon) {
		width: 2rem;
		height: 2rem;
		flex: none;
		color: var(--dn-title);
	}
	.ns__help-title {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 700;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--dn-title);
	}
	.ns__help-line {
		margin-top: 0.125rem;
	}

	@media (max-width: 1023px) {
		.ns {
			grid-template-columns: 9rem minmax(0, 1fr);
		}
	}

	@media (max-width: 639px) {
		.ns {
			grid-template-columns: 1fr;
			padding-top: 6.5rem;
		}
		.ns__art {
			position: absolute;
			top: 0;
			left: 50%;
			width: 12rem;
			height: 9rem;
			margin: 0;
			translate: -50% 0;
		}
		.ns__sheet {
			inset: 0.5rem 1rem 1rem;
			transform: rotate(-4deg);
		}
		.ns__art :global(.ns__deck) {
			top: auto;
			right: 50%;
			bottom: -2rem;
			height: auto;
			width: 7rem;
			translate: 50% 0;
			rotate: 90deg;
		}
		.ns :global(.ns__card > .card-body) {
			padding: 1.75rem 1.25rem 1.5rem;
		}
		.ns__title {
			font-size: 2.5rem;
		}
		.ns__line {
			font-size: 1rem;
		}
		.ns :global(.ns__go) {
			align-self: stretch;
		}
		.ns__gets {
			flex-direction: column;
			gap: 0.75rem;
		}
		.ns__gets li + li {
			padding-left: 0;
			border-left: 0;
		}
		.ns__help {
			align-items: flex-start;
			gap: 0.875rem;
			padding: 0.875rem 1rem;
		}
	}

	@media (prefers-contrast: more) {
		.ns__sheet {
			display: none;
		}
	}
</style>
