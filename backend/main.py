import os
import argparse
from dotenv import load_dotenv
from openai import OpenAI

def main():

    # To load enviroment variables
    load_dotenv()
    api_key = os.environ.get('OPENROUTER_API_KEY')
    if api_key == None:
        raise RuntimeError('API key nor found please write your OpenRouter API key in .env')

    # Handles command line argument
    parser = argparse.ArgumentParser(description="Rozu AI Agent Bot")
    parser.add_argument("user_prompt", type=str, help="User prompt")
    parser.add_argument("--verbose", action="store_true", help="Enable verbose output")
    args = parser.parse_args()

    # Creating a client
    client = OpenAI(
        base_url='https://openrouter.ai/api/v1',
        api_key=api_key,
    )

    # Get a response from the model
    messages = [
        {"role": "user", "content": args.user_prompt},
    ]

    response = client.chat.completions.create(
        model="openrouter/free",
        messages=messages,
    )

    if args.verbose:
        print(f'User prompt: {args.user_prompt}')
        print(f'Prompt tokens: {response.usage.prompt_tokens}')
        print(f'Response tokens: {response.usage.completion_tokens}')
    else:
        print(response.choices[0].message.content)

if __name__ == "__main__":
    main()
