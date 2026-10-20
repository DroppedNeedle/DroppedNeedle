<script lang="ts">
	import { Trash2, TriangleAlert, Users, Waypoints } from 'lucide-svelte';

	import { toastStore } from '$lib/stores/toast';
	import {
		getAdminAppPasswordsQuery,
		getConnectAppsSettingsQuery
	} from '$lib/queries/connect-apps/ConnectAppsQueries.svelte';
	import {
		adminRevokeAppPassword,
		saveConnectAppsSettings
	} from '$lib/queries/connect-apps/ConnectAppsMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { AdminAppPasswordView } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const rosterQuery = getAdminAppPasswordsQuery();
	const adminRevoke = adminRevokeAppPassword();

	const section = createSettingsSection(getConnectAppsSettingsQuery(), saveConnectAppsSettings(), {
		// A cleared number field binds to null; never send null.
		normalize: (draft) => ({
			...draft,
			transcode_max_bitrate_kbps: draft.transcode_max_bitrate_kbps ?? 320
		})
	});

	let pendingRevoke = $state<AdminAppPasswordView | null>(null);

	async function confirmRevoke() {
		const target = pendingRevoke;
		if (!target) return;
		try {
			await adminRevoke.mutateAsync({ id: target.id, userId: target.user_id });
			toastStore.show({ message: `Revoked "${target.name}"`, type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not revoke app-password', type: 'error' });
		} finally {
			pendingRevoke = null;
		}
	}

	function formatDate(unixSeconds: number | null | undefined): string {
		if (!unixSeconds) return 'Never';
		return new Date(unixSeconds * 1000).toLocaleDateString();
	}

	const rosterCount = $derived(rosterQuery.data?.length ?? 0);
</script>

<section class="flex flex-col gap-6">
	<header>
		<Card variant="bordered">
			<div class="flex items-center gap-2 text-accent">
				<Waypoints class="h-5 w-5" aria-hidden="true" />
				<h2 class="text-xl font-bold tracking-tight">Connect Apps</h2>
			</div>
			<p class="max-w-2xl text-base text-base-content/70">
				Let music apps like Symfonium and Finamp stream this library over the OpenSubsonic or
				Jellyfin protocols.
			</p>
			<p class="max-w-2xl text-sm text-base-content/50">
				These are server-wide switches. Each person creates their own app-password from their
				<a href={withBasePath('/profile#connect-apps')} class="link link-accent"
					>Profile → Connect Apps</a
				>.
			</p>
		</Card>
	</header>

	<Card variant="bordered">
		{#snippet header()}
			<CardTitle as="h3" class="text-base">Protocols</CardTitle>
		{/snippet}
		<QueryBoundary query={section} what="the Connect Apps settings">
			{#snippet children(draft)}
				<div class="mt-3 flex flex-col gap-5">
					<ToggleRow
						label="OpenSubsonic API"
						description="Symfonium, Feishin, Amperfy and other Subsonic apps."
						bind:checked={draft.subsonic_enabled}
					/>

					<ToggleRow
						label="Jellyfin-compatible API"
						description="Finamp, Jellify, Manet Music and Symfonium's Jellyfin mode."
						bind:checked={draft.jellyfin_enabled}
					/>

					<div class="divider my-0"></div>

					<ToggleRow label="On-the-fly transcoding" bind:checked={draft.transcoding_enabled} />

					{#if draft.transcoding_enabled}
						<div class="grid gap-4 sm:grid-cols-2">
							<SelectField
								label="Default format"
								bind:value={draft.transcode_default_format}
								options={[
									{ value: 'mp3', label: 'MP3' },
									{ value: 'opus', label: 'Opus' }
								]}
							/>
							<NumberField
								label="Max bitrate (kbps)"
								min={32}
								max={1411}
								bind:value={draft.transcode_max_bitrate_kbps}
							/>
						</div>
					{/if}

					<SaveBar {...section.bar} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>

	<!-- oversight: every user's connected apps (metadata only, never the secret) -->
	<Card variant="bordered">
		<div class="flex flex-col gap-4">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<CardTitle as="h3" class="text-base">
					<Users class="h-4 w-4" aria-hidden="true" />
					Connected app-passwords
				</CardTitle>
				<span class="badge badge-ghost" aria-label="connected app-password count">
					{rosterCount} active
				</span>
			</div>
			<p class="text-sm text-base-content/60">
				Every app-password across all users. You can revoke any of them, but the secret itself is
				never shown to anyone after it's created.
			</p>

			{#if rosterQuery.isLoading}
				<div class="skeleton h-24 w-full rounded-box"></div>
			{:else if rosterQuery.isError}
				<div class="alert alert-error" role="alert">
					<TriangleAlert class="h-5 w-5" aria-hidden="true" />
					<span>Could not load the app-password list.</span>
					<Button size="sm" onclick={() => void rosterQuery.refetch()}>Try again</Button>
				</div>
			{:else if rosterQuery.data && rosterQuery.data.length > 0}
				<div class="overflow-x-auto">
					<table class="table table-sm">
						<thead>
							<tr>
								<th>Owner</th>
								<th>Name</th>
								<th>Created</th>
								<th>Last used</th>
								<th>Last client</th>
								<th class="text-right">Action</th>
							</tr>
						</thead>
						<tbody>
							{#each rosterQuery.data as pw (pw.id)}
								<tr>
									<td>
										<span class="font-medium">{pw.owner_display_name}</span>
										<span class="block text-xs text-base-content/50">@{pw.owner_username}</span>
									</td>
									<td class="font-medium">{pw.name}</td>
									<td>{formatDate(pw.created_at)}</td>
									<td>{formatDate(pw.last_used_at)}</td>
									<td>{pw.last_client ?? '-'}</td>
									<td class="text-right">
										<Button
											variant="danger-ghost"
											size="xs"
											icon={Trash2}
											aria-label={`Revoke ${pw.name} for ${pw.owner_username}`}
											onclick={() => (pendingRevoke = pw)}
										>
											Revoke
										</Button>
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}
				<div
					class="rounded-box border border-dashed border-base-300 p-6 text-center text-base-content/60"
				>
					No app-passwords have been created yet.
				</div>
			{/if}
		</div>
	</Card>

	<ConfirmDialog
		bind:open={
			() => pendingRevoke !== null,
			(open) => {
				if (!open) pendingRevoke = null;
			}
		}
		title={`Revoke "${pendingRevoke?.name ?? ''}" for ${pendingRevoke?.owner_display_name ?? ''}?`}
		message="This disconnects their app immediately; they can create a new one from their Profile."
		confirmLabel="Revoke"
		tone="danger"
		pending={adminRevoke.isPending}
		onconfirm={confirmRevoke}
	/>
</section>
