<script lang="ts">
	import '$lib/components/release-notes.css';
	import {
		getVersionQuery,
		getUpdateCheckQuery,
		getReleaseHistoryQuery
	} from '$lib/queries/VersionQuery.svelte';
	import SafeHtml from '$lib/components/SafeHtml.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { ExternalLink, RefreshCw, Info, Tag, Calendar, CircleArrowUp } from 'lucide-svelte';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	const versionQuery = getVersionQuery();
	const updateCheckQuery = getUpdateCheckQuery();
	const releaseHistoryQuery = getReleaseHistoryQuery();

	const version = $derived(versionQuery.data);
	const updateCheck = $derived(updateCheckQuery.data);
	const releases = $derived(releaseHistoryQuery.data);
	const isCheckingUpdate = $derived(updateCheckQuery.isFetching);

	function formatDate(dateStr: string): string {
		return new Date(dateStr).toLocaleDateString('en-US', {
			year: 'numeric',
			month: 'long',
			day: 'numeric'
		});
	}
</script>

<div class="space-y-6 stagger-fade-in">
	<div>
		<h2 class="text-2xl font-bold">About</h2>
		<p class="text-base-content/60 mt-1">App version, updates, and release notes.</p>
	</div>

	<Card variant="bordered" padding="md">
		<p class="text-xs text-base-content/60">
			This application uses Discogs’ API but is not affiliated with, sponsored or endorsed by
			Discogs. ‘Discogs’ is a trademark of Zink Media, LLC.
		</p>
	</Card>

	{#if versionQuery.isLoading}
		<div class="flex justify-center items-center py-20">
			<span class="loading loading-spinner loading-lg text-primary"></span>
		</div>
	{:else if versionQuery.isError}
		<Card variant="bordered" class="text-center">
			<div class="flex flex-col items-center">
				<Info class="w-10 h-10 text-base-content/50 mb-2" />
				<p class="text-base-content/70">Unable to load version information.</p>
				<Button variant="primary" size="sm" class="mt-3" onclick={() => versionQuery.refetch()}>
					Try Again
				</Button>
			</div>
		</Card>
	{:else if version}
		<Card variant="bordered" class="relative overflow-hidden">
			<div
				class="absolute inset-0 bg-gradient-to-br from-primary/5 via-transparent to-accent/5 pointer-events-none"
			></div>
			<div class="relative">
				<div class="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4">
					<div class="flex items-center gap-4">
						<div class="bg-primary/10 p-3 rounded-xl">
							<Info class="w-7 h-7 text-primary" />
						</div>
						<div>
							<h3 class="text-xl font-bold">DroppedNeedle</h3>
							<div class="flex items-center gap-2 mt-1">
								<span class="badge badge-accent font-mono">
									{version.version}
								</span>
								{#if version.build_date}
									<span class="flex items-center gap-1 text-xs text-base-content/60">
										<Calendar class="w-3 h-3" />
										Built {formatDate(version.build_date)}
									</span>
								{/if}
							</div>
						</div>
					</div>

					<div class="flex flex-wrap gap-2">
						<Button
							variant="primary"
							size="sm"
							icon={RefreshCw}
							loading={isCheckingUpdate}
							onclick={() => updateCheckQuery.refetch()}
						>
							Check for Updates
						</Button>
						<Button
							variant="ghost"
							size="sm"
							href="https://github.com/DroppedNeedle/DroppedNeedle"
							target="_blank"
							rel="noopener noreferrer"
						>
							<GitHubIcon class="w-4 h-4" />
							View on GitHub
							<ExternalLink class="w-3 h-3" />
						</Button>
					</div>
				</div>

				{#if updateCheck}
					{#if updateCheck.update_available && updateCheck.latest_version}
						<div class="alert alert-info alert-soft mt-4">
							<CircleArrowUp class="w-5 h-5 shrink-0" />
							<div>
								<p class="font-semibold">
									Update available: <span class="text-accent">{updateCheck.latest_version}</span>
								</p>
								<p class="text-sm opacity-80">
									{#if updateCheck.comparison_failed}
										Simulated update - dev build can't compare versions.
									{:else}
										You're on {updateCheck.current_version}. A newer version is ready.
									{/if}
								</p>
							</div>
							{#if outsideLink(updateCheck.latest_release?.html_url)}
								<Button
									variant="ghost"
									size="sm"
									class="ml-auto"
									href={outsideLink(updateCheck.latest_release?.html_url)}
									target="_blank"
									rel="noopener noreferrer"
								>
									View Release
									<ExternalLink class="w-3 h-3" />
								</Button>
							{/if}
						</div>
					{:else if updateCheck.comparison_failed}
						<div class="alert alert-warning alert-soft mt-4">
							<Info class="w-5 h-5 shrink-0" />
							<span>Couldn't compare versions - unrecognized format.</span>
						</div>
					{:else}
						<div class="alert alert-success alert-soft mt-4">
							<CircleArrowUp class="w-5 h-5 shrink-0" />
							<span>You're on the latest version.</span>
						</div>
					{/if}
				{:else if updateCheckQuery.isError}
					<div class="alert alert-warning alert-soft mt-4">
						<Info class="w-5 h-5 shrink-0" />
						<span>Couldn't check for updates.</span>
					</div>
				{/if}
			</div>
		</Card>
	{/if}

	{#if updateCheck?.update_available && updateCheck.latest_release}
		<Card variant="bordered">
			<div>
				<div class="flex items-center gap-3 mb-4">
					<div class="bg-accent/10 p-2 rounded-lg">
						<Tag class="w-5 h-5 text-accent" />
					</div>
					<div>
						<h3 class="font-semibold text-lg">
							What's New in <span class="text-accent">{updateCheck.latest_version}</span>
						</h3>
						{#if updateCheck.latest_release.published_at}
							<p class="text-xs text-base-content/60">
								Released {formatDate(updateCheck.latest_release.published_at)}
							</p>
						{/if}
					</div>
				</div>
				{#if updateCheck.latest_release.body}
					<SafeHtml
						markdown={updateCheck.latest_release.body}
						class="release-notes-prose prose prose-sm max-w-none text-base-content/80"
					/>
				{/if}
			</div>
		</Card>
	{/if}

	<div>
		<div class="flex items-center gap-3 mb-4">
			<h3 class="text-lg font-semibold">Release History</h3>
		</div>

		{#if releaseHistoryQuery.isLoading}
			<div class="space-y-3">
				{#each Array(3) as _, i (i)}
					<Card variant="bordered" padding="md" class="skeleton-shimmer">
						<div class="flex items-center gap-3">
							<div class="h-5 w-16 bg-base-content/10 rounded"></div>
							<div class="h-4 w-40 bg-base-content/10 rounded"></div>
							<div class="ml-auto h-3 w-24 bg-base-content/10 rounded"></div>
						</div>
					</Card>
				{/each}
			</div>
		{:else if releaseHistoryQuery.isError}
			<Card variant="bordered" class="text-center">
				<div class="flex flex-col items-center">
					<Info class="w-8 h-8 text-base-content/50 mb-2" />
					<p class="text-base-content/70">Unable to load release history.</p>
					<Button
						variant="primary"
						size="sm"
						class="mt-3"
						onclick={() => releaseHistoryQuery.refetch()}
					>
						Try Again
					</Button>
				</div>
			</Card>
		{:else if releases && releases.length > 0}
			<div class="space-y-3 border-l-2 border-accent/20 pl-6">
				{#each releases as release (release.tag_name)}
					<div
						class="collapse collapse-arrow bg-base-200 rounded-box shadow-md border border-base-content/5 hover:shadow-lg hover:border-accent/10 transition-all duration-200 relative"
					>
						<div
							class="absolute -left-[1.75rem] top-5 w-2.5 h-2.5 rounded-full bg-accent/40 ring-2 ring-base-100"
						></div>
						<input
							type="checkbox"
							aria-label="Toggle {release.name ?? release.tag_name} release notes"
						/>
						<div class="collapse-title">
							<div class="flex items-center gap-3 flex-wrap">
								<span class="badge badge-accent badge-sm font-mono">{release.tag_name}</span>
								{#if release.prerelease}
									<span class="badge badge-ghost badge-sm">pre-release</span>
								{/if}
								<span class="text-sm font-medium">{release.name ?? release.tag_name}</span>
								{#if release.published_at}
									<span class="ml-auto text-xs text-base-content/60 flex items-center gap-1">
										<Calendar class="w-3 h-3" />
										{formatDate(release.published_at)}
									</span>
								{/if}
							</div>
						</div>
						<div class="collapse-content">
							{#if release.body}
								<SafeHtml
									markdown={release.body}
									class="release-notes-prose prose prose-sm max-w-none text-base-content/80 pt-2"
								/>
							{/if}
							{#if outsideLink(release.html_url)}
								<div class="mt-4 pt-3 border-t border-base-content/10">
									<a
										href={outsideLink(release.html_url)}
										target="_blank"
										rel="noopener noreferrer"
										class="link link-primary text-sm inline-flex items-center gap-1"
									>
										View on GitHub
										<ExternalLink class="w-3 h-3" />
									</a>
								</div>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{:else}
			<Card variant="bordered" class="text-center">
				<div class="flex flex-col items-center py-2">
					<Tag class="w-8 h-8 text-base-content/50 mb-2" />
					<p class="text-base-content/70">No releases found.</p>
				</div>
			</Card>
		{/if}
	</div>
</div>
