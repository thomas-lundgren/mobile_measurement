package com.measurement.logger;

import static org.junit.Assert.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory;
import com.networknt.schema.InputFormat;
import com.networknt.schema.JsonSchema;
import com.networknt.schema.JsonSchemaFactory;
import com.networknt.schema.SchemaLocation;
import com.networknt.schema.SpecVersion;
import com.networknt.schema.ValidationMessage;

import org.json.JSONArray;
import org.json.JSONObject;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Set;

/** The repo's contracts/ for the unit tests: files, JSON Schema and OpenAPI validation. */
final class Contracts {
    private Contracts() {}

    private static final String SCHEMA_BASE = "https://measurement-platform/contracts/schemas/";
    private static final ObjectMapper JSON = new ObjectMapper();

    static Path dir() {
        String d = System.getProperty("contracts.dir");
        return d != null ? Paths.get(d) : Paths.get("../../contracts");
    }

    static String text(String rel) throws Exception {
        return new String(Files.readAllBytes(dir().resolve(rel)), StandardCharsets.UTF_8);
    }

    static JSONObject json(String rel) throws Exception {
        return new JSONObject(text(rel));
    }

    /** Same JSON value (key order ignored). */
    static boolean same(Object a, Object b) throws Exception {
        return JSON.readTree(new JSONArray().put(a).toString()).equals(JSON.readTree(new JSONArray().put(b).toString()));
    }

    static byte[] hex(String s) {
        byte[] out = new byte[s.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(s.substring(2 * i, 2 * i + 2), 16);
        }
        return out;
    }

    private static final JsonSchemaFactory FACTORY = JsonSchemaFactory.getInstance(
            SpecVersion.VersionFlag.V202012,
            b -> b.schemaMappers(m -> m.mapPrefix(SCHEMA_BASE, dir().resolve("schemas").toUri().toString())));

    /** Fails unless {@code body} is valid against contracts/schemas/{@code file}. */
    static void assertValid(String file, JSONObject body) throws Exception {
        JsonSchema schema = FACTORY.getSchema(SchemaLocation.of(SCHEMA_BASE + file));
        report(file, schema.validate(body.toString(), InputFormat.JSON), body);
    }

    /** Fails unless {@code body} is valid against the OpenAPI component {@code name}. */
    static void assertValidOpenApi(String name, JSONObject body) throws Exception {
        JsonNode openapi = new ObjectMapper(new YAMLFactory()).readTree(text("openapi/openapi.yaml"));
        ObjectNode root = JSON.createObjectNode();
        root.put("$schema", "https://json-schema.org/draft/2020-12/schema");
        root.put("$ref", "#/components/schemas/" + name);
        root.set("components", openapi.get("components"));
        JsonSchema schema = FACTORY.getSchema(root);
        report(name, schema.validate(body.toString(), InputFormat.JSON), body);
    }

    private static void report(String what, Set<ValidationMessage> errors, JSONObject body) throws Exception {
        assertTrue(what + ": " + errors + "\n" + body.toString(2), errors.isEmpty());
    }
}
