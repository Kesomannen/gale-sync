# API Docs

Base URL: `https://gale.kesomannen.com/api`

## Authentication

Discord OAuth is used to authenticate users.

Once the user is authenticated, two tokens are created:

- The `access token` is a [JWT](https://jwt.io) that is used to authorize the user when accessing restricted endpoints. This token expires quickly (currently 30 minutes).
- The `refresh token` is used to request a new access token once it expires. It is issued by Discord and much longer lived.

To use restricted endpoints, include the access token as a header:

```http
Authorization: Bearer <token>
```

## Errors

API errors are always returned as a JSON object with the following format:

```ts
type ApiError = {
  message: string;
};
```

## Enpoints

### `GET /auth/login`

Begins the discord OAuth flow.

> [!NOTE]
> The authentication flow isn't yet compatible with applications other than Gale, although this is being worked on.

**Response**

`302 Redirect` to the Discord OAuth page. This should be opened in the user's browser.

### `GET /auth/callback`

Callback for Discord OAuth. Should not be called directly.

**Query Parameters**

```ts
type CallbackParameters = {
  code: string; // authorization code to exchange for token
  state: string; // OAuth state parameter
};
```

**Response**

`302 Redirect` to `gale://auth/callback?access_token=Xrefresh_token=X`. This deep link is handled by the Gale app to receive the token.

### `POST /auth/token`

Consumes the refresh token to grant new auth tokens.

**Request body**

```ts
type TokenRequest = {
  refreshToken: string;
};
```

**Response**

`200 OK`

```ts
type TokenResponse = {
  accessToken: string;
  refreshToken: string;
};
```

> [!NOTE]
> Once you call this endpoint, the same request token cannot be used again.

### `POST /profile`

Creates a new synced profile.

Requires Authorization.

**Request**

A ZIP-archive (MIME-type `application/zip`) that contains the profile's manifest and any config files.

The manifest is a **YAML file** named `export.r2x`. The schema mimicks r2modman's export schema (see [Types](#types)).

The max size is currently `2 MiB` (`~2.1 MB`).

**Response**

`204 CREATED`

```ts
type CreateProfileResponse = {
  id: string;
  createdAt: string; // ISO8601
  updatedAt: string; // ISO8601
};
```

### `GET /profile/{id}`

Downloads a synced profile.

**Response**

`302 Redirect` to the profile's CDN endpoint.

### `PUT /profile/{id}`

Updates a synced profile.

Requires Authorization.

**Request**

Same as [`POST /profile`](#post-profile). Note that the `profileName` does not have to be consistent across updates.

**Response**

`204 CREATED`

```ts
type UpdateProfileResponse = {
  id: string;
  createdAt: string; // ISO8601
  updatedAt: string; // ISO8601
};
```

### `DELETE /profile/{id}`

Deletes a synced profile.

Requires Authorization.

**Response**

`201 NO CONTENT`

### `GET /profile/{id}/meta`

Returns metadata about a synced profile.

**Response**

```ts
type ProfileMetadata = {
  id: string;
  createdAt: string;
  updatedAt: string;
  owner: User;
  manifest: ProfileManifest;
};
```

**Example response**

```json
{
  "id": "GsioqKpVRwiP7_ynX-QsuA",
  "createdAt": "2025-04-25T07:08:52.076422Z",
  "updatedAt": "2025-04-25T08:33:22.669857Z",
  "owner": {
    "discordId": "308117922260451300",
    "name": "kesomannen",
    "displayName": "Bobbo ::)",
    "avatar": null
  },
  "manifest": {
    "profileName": "Default",
    "community": "repo",
    "mods": [
      {
        "name": "BepInEx-BepInExPack",
        "enabled": true,
        "version": {
          "major": 5,
          "minor": 4,
          "patch": 2100
        }
      }
    ]
  }
}
```

### `GET /user/me`

Returns information about the current user.

Requires Authorization.

**Response**

```ts
type UserWithProfiles = User & {
  profiles: {
    id: string;
    name: string;
    community: string;
    createdAt: string; // ISO8601
    updatedAt: string; // ISO8601
  }[];
};
```

**Example response**

```json
{
    "discordId": "308117922260451340",
    "name": "kesomannen",
    "displayName": "Bobbo ::)",
    "avatar": "0d148b55b680b38fe207988e2d3bbfd0",
    "profiles": [
        {
            "id": "SXfMJaBKQq2UEwCyScQcSQ",
            "name": "Default",
            "community": "repo",
            "createdAt": "2025-05-14T19:04:00.753826Z",
            "updatedAt": "2025-05-14T19:04:00.753826Z"
        },
        {
            "id": "vHT-YZa2R5yUmTXnHIb0Qg",
            "name": "My Profile",
            "community": "lethal-company",
            "createdAt": "2025-05-19T14:10:11.836673Z",
            "updatedAt": "2025-05-21T15:30:03.239542Z"
        }
    ]
}
```

## Types

### `User`

```ts
type User = {
  discordId: string;
  name: string;
  displayName: string;
  avatar: string | null; // Discord CDN hash
};
```

### `ProfileManifest`

```ts
type ProfileManifest = {
  profileName: string;
  community?: string | null; // URL slug of a Thunderstore community
  mods: {
    name: string; // formatted as `namespace-name`
    enabled: boolean;
    version: {
      major: number;
      minor: number;
      patch: number;
    };
  }[];
};
```
